// Adapted from Celmux's MIT-licensed ES9+ protocol client.
// Copyright (c) 2026 anti-rainer
// Copyright (c) 2026 Proletarius contributors
// SPDX-License-Identifier: MIT

//! Switch-owned ES9+ HTTPS adapter for the transport-independent LPA flow.
//!
//! This follows the existing Go client's explicit GSMA CI trust bundle,
//! `gsma/rsp/v2.5.0` protocol header, direct route, bounded response bodies,
//! and no-redirect policy. Requests are blocking because the LPA flow also
//! performs synchronous APDU work; invoke it on a dedicated blocking worker.

use std::{io::Read, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{Certificate, blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;

use crate::{
    ActivationCode,
    es9::{AuthenticateResponse, BoundProfilePackage, Es9Transport, HttpError, InitiateResponse},
};

const ADMIN_PROTOCOL: &str = "gsma/rsp/v2.5.0";
const USER_AGENT: &str = "gsma-rsp-lpad";
const MAX_CONTROL_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_BPP_RESPONSE_BYTES: usize = 24 * 1024 * 1024;
const MAX_BPP_BYTES: usize = 16 * 1024 * 1024;
const MAX_TLV_BYTES: usize = 65_535;
const MAX_TRANSACTION_ID_BYTES: usize = 64;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

const GSMA_CI_ROOTS: &[u8] = include_bytes!("../resources/rootci/bundle.pem");
const GSMA_TEST_CI_ROOTS: &[u8] = include_bytes!("../resources/rootci/bundle-tests.pem");

/// Blocking network adapter matching Switch's existing ES9+ TLS trust policy.
///
/// The client trusts only the GSMA CI roots already shipped with Switch. The
/// eUICC separately verifies the signed server certificate during ES10b
/// authentication. TLS verification remains enabled for the HTTPS endpoint.
pub struct HttpEs9Client {
    client: Client,
}

impl HttpEs9Client {
    pub fn new() -> Result<Self, HttpError> {
        let roots = gsma_ci_certificates()?;
        let client = Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            .no_proxy()
            .tls_certs_only(roots)
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(|_| HttpError::Transport("could not initialize ES9+ HTTPS client".into()))?;
        Ok(Self { client })
    }

    fn post_json<I, O>(
        &self,
        url: &str,
        input: &I,
        response_limit: usize,
        timeout: Duration,
    ) -> Result<O, HttpError>
    where
        I: Serialize,
        O: DeserializeOwned + HasHeader,
    {
        let response = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header("X-Admin-Protocol", ADMIN_PROTOCOL)
            .timeout(timeout)
            .json(input)
            .send()
            .map_err(|_| HttpError::Transport("ES9+ HTTPS request failed".into()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(HttpError::Status(status.as_u16()));
        }
        let body = read_bounded(response, response_limit)?;
        decode_success_response(&body)
    }

    fn post_notification(
        &self,
        url: &str,
        notification: &[u8],
        timeout: Duration,
    ) -> Result<(), HttpError> {
        let response = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header("X-Admin-Protocol", ADMIN_PROTOCOL)
            .timeout(timeout)
            .json(&NotificationRequest {
                notification: B64(notification),
            })
            .send()
            .map_err(|_| HttpError::Transport("ES9+ notification request failed".into()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(HttpError::Status(status.as_u16()));
        }
        let body = read_bounded(response, MAX_CONTROL_RESPONSE_BYTES)?;
        if !body.is_empty() {
            serde_json::from_slice::<NotificationResponse>(&body)
                .map_err(|_| HttpError::Json("invalid ES9+ notification response".into()))?;
        }
        Ok(())
    }
}

impl Es9Transport for HttpEs9Client {
    type Error = HttpError;

    fn initiate(
        &self,
        activation: &ActivationCode,
        challenge: &[u8],
        euicc_info1: &[u8],
        timeout: Duration,
    ) -> Result<InitiateResponse, Self::Error> {
        ensure_tlv_size(euicc_info1)?;
        let response: InitiateJson = self.post_json(
            &activation_endpoint(activation, "initiateAuthentication")?,
            &InitiateRequest {
                challenge: B64(challenge),
                euicc_info1: B64(euicc_info1),
                smdp_address: activation.server_address(),
            },
            MAX_CONTROL_RESPONSE_BYTES,
            timeout,
        )?;
        let server_transaction_id = validate_server_transaction_id(&response.transaction_id)?;
        let transaction_id = decode_transaction_id(&server_transaction_id)?;
        Ok(InitiateResponse {
            server_transaction_id,
            transaction_id,
            signed1: response.signed1.into_bytes(MAX_TLV_BYTES)?,
            signature1: response.signature1.into_bytes(MAX_TLV_BYTES)?,
            euicc_ci_pk_id: response.euicc_ci_pk_id.into_bytes(MAX_TLV_BYTES)?,
            server_certificate: response.server_certificate.into_bytes(MAX_TLV_BYTES)?,
        })
    }

    fn authenticate_client(
        &self,
        activation: &ActivationCode,
        server_transaction_id: &str,
        authenticate_server_response: &[u8],
        timeout: Duration,
    ) -> Result<AuthenticateResponse, Self::Error> {
        validate_server_transaction_id(server_transaction_id)?;
        ensure_tlv_size(authenticate_server_response)?;
        let response: AuthenticateJson = self.post_json(
            &activation_endpoint(activation, "authenticateClient")?,
            &AuthenticateRequest {
                transaction_id: server_transaction_id,
                response: B64(authenticate_server_response),
            },
            MAX_CONTROL_RESPONSE_BYTES,
            timeout,
        )?;
        ensure_same_server_transaction(server_transaction_id, &response.transaction_id)?;
        Ok(AuthenticateResponse {
            server_transaction_id: response.transaction_id,
            transaction_id: decode_transaction_id(server_transaction_id)?,
            profile_metadata: response.profile_metadata.into_bytes(MAX_TLV_BYTES)?,
            signed2: response.signed2.into_bytes(MAX_TLV_BYTES)?,
            signature2: response.signature2.into_bytes(MAX_TLV_BYTES)?,
            server_certificate: response.server_certificate.into_bytes(MAX_TLV_BYTES)?,
        })
    }

    fn get_bound_profile_package(
        &self,
        activation: &ActivationCode,
        server_transaction_id: &str,
        prepare_download_response: &[u8],
        timeout: Duration,
    ) -> Result<BoundProfilePackage, Self::Error> {
        validate_server_transaction_id(server_transaction_id)?;
        ensure_tlv_size(prepare_download_response)?;
        let response: BppJson = self.post_json(
            &activation_endpoint(activation, "getBoundProfilePackage")?,
            &BppRequest {
                transaction_id: server_transaction_id,
                response: B64(prepare_download_response),
            },
            MAX_BPP_RESPONSE_BYTES,
            timeout,
        )?;
        ensure_same_server_transaction(server_transaction_id, &response.transaction_id)?;
        Ok(BoundProfilePackage {
            server_transaction_id: response.transaction_id,
            transaction_id: decode_transaction_id(server_transaction_id)?,
            package: response.package.into_bytes(MAX_BPP_BYTES)?,
        })
    }

    fn handle_notification(
        &self,
        address: &str,
        notification: &[u8],
        timeout: Duration,
    ) -> Result<(), Self::Error> {
        ensure_tlv_size(notification)?;
        let url = notification_endpoint(address)?;
        self.post_notification(&url, notification, timeout)
    }

    fn cancel_session(
        &self,
        activation: &ActivationCode,
        server_transaction_id: &str,
        cancel_response: &[u8],
        timeout: Duration,
    ) -> Result<(), Self::Error> {
        validate_server_transaction_id(server_transaction_id)?;
        ensure_tlv_size(cancel_response)?;
        let _: CancelJson = self.post_json(
            &activation_endpoint(activation, "cancelSession")?,
            &CancelRequest {
                transaction_id: server_transaction_id,
                response: B64(cancel_response),
            },
            MAX_CONTROL_RESPONSE_BYTES,
            timeout,
        )?;
        Ok(())
    }
}

fn gsma_ci_certificates() -> Result<Vec<Certificate>, HttpError> {
    let mut certificates = Certificate::from_pem_bundle(GSMA_CI_ROOTS)
        .map_err(|_| HttpError::Invalid("GSMA CI trust bundle is invalid"))?;
    certificates.extend(
        Certificate::from_pem_bundle(GSMA_TEST_CI_ROOTS)
            .map_err(|_| HttpError::Invalid("GSMA test CI trust bundle is invalid"))?,
    );
    if certificates.is_empty() {
        return Err(HttpError::Invalid("GSMA CI trust bundle is empty"));
    }
    Ok(certificates)
}

fn activation_endpoint(activation: &ActivationCode, operation: &str) -> Result<String, HttpError> {
    let url = Url::parse(&format!(
        "https://{}/gsma/rsp2/es9plus/{operation}",
        activation.server_address()
    ))
    .map_err(|_| HttpError::Invalid("SM-DP+ address is invalid"))?;
    validate_https_endpoint(&url)?;
    if !matches!(
        operation,
        "initiateAuthentication"
            | "authenticateClient"
            | "getBoundProfilePackage"
            | "cancelSession"
    ) {
        return Err(HttpError::Invalid("ES9+ operation is invalid"));
    }
    Ok(url.into())
}

fn notification_endpoint(address: &str) -> Result<String, HttpError> {
    if address.len() > 1024 || address.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(HttpError::Invalid("notification address is invalid"));
    }
    // Preserve the existing euicc-go workaround for a known server that
    // returns an ASCII space in the authority while rejecting URL injection.
    let normalized = address.replace(' ', "");
    let candidate = if normalized.contains("://") {
        normalized
    } else {
        if normalized.contains('/') || normalized.contains('@') {
            return Err(HttpError::Invalid("notification address is invalid"));
        }
        format!("https://{normalized}")
    };
    let mut url =
        Url::parse(&candidate).map_err(|_| HttpError::Invalid("notification URL is invalid"))?;
    validate_https_endpoint(&url)?;
    url.set_path("/gsma/rsp2/es9plus/handleNotification");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

fn validate_https_endpoint(url: &Url) -> Result<(), HttpError> {
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(HttpError::Invalid(
            "ES9+ endpoint must be HTTPS without credentials",
        ));
    }
    Ok(())
}

fn read_bounded(
    mut response: reqwest::blocking::Response,
    limit: usize,
) -> Result<Vec<u8>, HttpError> {
    let mut body =
        Vec::with_capacity(response.content_length().unwrap_or(0).min(limit as u64) as usize);
    response
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|_| HttpError::Transport("could not read ES9+ response".into()))?;
    if body.len() > limit {
        return Err(HttpError::TooLarge);
    }
    Ok(body)
}

fn decode_success_response<O>(body: &[u8]) -> Result<O, HttpError>
where
    O: DeserializeOwned + HasHeader,
{
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| HttpError::Json("invalid ES9+ response JSON".into()))?;
    let header = value
        .get("header")
        .cloned()
        .ok_or(HttpError::Invalid("function execution status is missing"))?;
    let header: Header = serde_json::from_value(header)
        .map_err(|_| HttpError::Json("invalid ES9+ response header".into()))?;
    header.ensure_success()?;
    let decoded: O = serde_json::from_value(value)
        .map_err(|_| HttpError::Json("invalid ES9+ response fields".into()))?;
    decoded.ensure_success()?;
    Ok(decoded)
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Header {
    #[serde(rename = "functionExecutionStatus")]
    execution_status: Option<ExecutionStatus>,
}

impl Header {
    fn ensure_success(&self) -> Result<(), HttpError> {
        let status = self
            .execution_status
            .as_ref()
            .ok_or(HttpError::Invalid("function execution status is missing"))?;
        if matches!(
            status.status.as_deref(),
            Some("Executed-Success" | "Executed-WithWarning")
        ) {
            return Ok(());
        }
        let data = status.data.as_ref();
        Err(HttpError::Remote {
            status: status.status.as_deref().unwrap_or("missing").to_owned(),
            subject_code: data.and_then(|value| value.subject_code.clone()),
            reason_code: data.and_then(|value| value.reason_code.clone()),
            message: data.and_then(|value| value.message.clone()),
            subject_identifier: data.and_then(|value| value.subject_identifier.clone()),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ExecutionStatus {
    status: Option<String>,
    #[serde(rename = "statusCodeData")]
    data: Option<StatusCodeData>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct StatusCodeData {
    #[serde(rename = "subjectCode")]
    subject_code: Option<String>,
    #[serde(rename = "reasonCode")]
    reason_code: Option<String>,
    message: Option<String>,
    #[serde(rename = "subjectIdentifier")]
    subject_identifier: Option<String>,
}

trait HasHeader {
    fn ensure_success(&self) -> Result<(), HttpError>;
}

#[derive(Deserialize)]
struct InitiateJson {
    header: Header,
    #[serde(rename = "transactionId")]
    transaction_id: String,
    #[serde(rename = "serverSigned1")]
    signed1: DecodedB64,
    #[serde(rename = "serverSignature1")]
    signature1: DecodedB64,
    #[serde(rename = "euiccCiPKIdToBeUsed")]
    euicc_ci_pk_id: DecodedB64,
    #[serde(rename = "serverCertificate")]
    server_certificate: DecodedB64,
}

impl HasHeader for InitiateJson {
    fn ensure_success(&self) -> Result<(), HttpError> {
        self.header.ensure_success()
    }
}

#[derive(Serialize)]
struct InitiateRequest<'a> {
    #[serde(rename = "euiccChallenge")]
    challenge: B64<'a>,
    #[serde(rename = "euiccInfo1")]
    euicc_info1: B64<'a>,
    #[serde(rename = "smdpAddress")]
    smdp_address: &'a str,
}

#[derive(Deserialize)]
struct AuthenticateJson {
    header: Header,
    #[serde(rename = "transactionId")]
    transaction_id: String,
    #[serde(rename = "profileMetadata")]
    profile_metadata: DecodedB64,
    #[serde(rename = "smdpSigned2")]
    signed2: DecodedB64,
    #[serde(rename = "smdpSignature2")]
    signature2: DecodedB64,
    #[serde(rename = "smdpCertificate")]
    server_certificate: DecodedB64,
}

impl HasHeader for AuthenticateJson {
    fn ensure_success(&self) -> Result<(), HttpError> {
        self.header.ensure_success()
    }
}

#[derive(Serialize)]
struct AuthenticateRequest<'a> {
    #[serde(rename = "transactionId")]
    transaction_id: &'a str,
    #[serde(rename = "authenticateServerResponse")]
    response: B64<'a>,
}

#[derive(Deserialize)]
struct BppJson {
    header: Header,
    #[serde(rename = "transactionId")]
    transaction_id: String,
    #[serde(rename = "boundProfilePackage")]
    package: DecodedB64,
}

impl HasHeader for BppJson {
    fn ensure_success(&self) -> Result<(), HttpError> {
        self.header.ensure_success()
    }
}

#[derive(Serialize)]
struct BppRequest<'a> {
    #[serde(rename = "transactionId")]
    transaction_id: &'a str,
    #[serde(rename = "prepareDownloadResponse")]
    response: B64<'a>,
}

#[derive(Serialize)]
struct NotificationRequest<'a> {
    #[serde(rename = "pendingNotification")]
    notification: B64<'a>,
}

#[derive(Deserialize)]
struct NotificationResponse {}

#[derive(Serialize)]
struct CancelRequest<'a> {
    #[serde(rename = "transactionId")]
    transaction_id: &'a str,
    #[serde(rename = "cancelSessionResponse")]
    response: B64<'a>,
}

#[derive(Deserialize)]
struct CancelJson {
    header: Header,
}

impl HasHeader for CancelJson {
    fn ensure_success(&self) -> Result<(), HttpError> {
        self.header.ensure_success()
    }
}

struct B64<'a>(&'a [u8]);

impl Serialize for B64<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(self.0))
    }
}

#[derive(Deserialize)]
#[serde(transparent)]
struct DecodedB64(String);

impl DecodedB64 {
    fn into_bytes(self, limit: usize) -> Result<Vec<u8>, HttpError> {
        if self.0.len() > limit.saturating_mul(4).div_ceil(3).saturating_add(4) {
            return Err(HttpError::TooLarge);
        }
        let compact: Vec<u8> = self
            .0
            .as_bytes()
            .iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        let decoded = STANDARD
            .decode(&compact)
            .map_err(|_| HttpError::Base64("invalid base64 ES9+ field".into()))?;
        if decoded.is_empty() {
            return Err(HttpError::Invalid("required ES9+ field is empty"));
        }
        if decoded.len() > limit {
            return Err(HttpError::TooLarge);
        }
        Ok(decoded)
    }
}

fn ensure_tlv_size(value: &[u8]) -> Result<(), HttpError> {
    if value.is_empty() || value.len() > MAX_TLV_BYTES {
        return Err(HttpError::Invalid("BER-TLV size is out of range"));
    }
    Ok(())
}

fn validate_server_transaction_id(input: &str) -> Result<String, HttpError> {
    if input.is_empty()
        || input.len() > MAX_TRANSACTION_ID_BYTES * 2
        || input.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(HttpError::Invalid("transaction ID encoding is invalid"));
    }
    Ok(input.to_owned())
}

fn ensure_same_server_transaction(expected: &str, actual: &str) -> Result<(), HttpError> {
    validate_server_transaction_id(actual)?;
    if !expected.eq_ignore_ascii_case(actual) {
        return Err(HttpError::Invalid(
            "transaction ID changed during the session",
        ));
    }
    Ok(())
}

fn decode_transaction_id(input: &str) -> Result<Vec<u8>, HttpError> {
    validate_server_transaction_id(input)?;
    if !input.len().is_multiple_of(2) {
        return Err(HttpError::Invalid("transaction ID is not hex"));
    }
    let mut output = Vec::with_capacity(input.len() / 2);
    for pair in input.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0]).ok_or(HttpError::Invalid("transaction ID is not hex"))?;
        let low = hex_nibble(pair[1]).ok_or(HttpError::Invalid("transaction ID is not hex"))?;
        output.push(high << 4 | low);
    }
    if output.is_empty() {
        return Err(HttpError::Invalid("transaction ID is empty"));
    }
    Ok(output)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        thread,
    };

    #[derive(Deserialize)]
    struct HeaderOnly {
        header: Header,
    }

    impl HasHeader for HeaderOnly {
        fn ensure_success(&self) -> Result<(), HttpError> {
            self.header.ensure_success()
        }
    }

    fn test_client() -> HttpEs9Client {
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .build()
            .unwrap();
        HttpEs9Client { client }
    }

    fn serve_once(status: u16, body: Vec<u8>) -> (String, thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let request = read_mock_request(&mut stream);
            let status_text = match status {
                200 => "OK",
                302 => "Found",
                503 => "Service Unavailable",
                _ => "Test Response",
            };
            let response = format!(
                "HTTP/1.1 {status} {status_text}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
            request
        });
        (format!("http://{address}/es9"), handle)
    }

    fn read_mock_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        let mut expected_length = None;
        loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "client closed before the request was complete");
            request.extend_from_slice(&buffer[..read]);
            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let body_start = header_end + 4;
            if expected_length.is_none() {
                let headers = std::str::from_utf8(&request[..header_end]).unwrap();
                expected_length = Some(
                    headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0),
                );
            }
            if request.len() >= body_start + expected_length.unwrap() {
                return request;
            }
        }
    }

    #[test]
    fn parses_gsma_ci_roots_without_disabling_tls_validation() {
        let roots = gsma_ci_certificates().unwrap();
        assert!(roots.len() >= 2);
    }

    #[test]
    fn execution_status_matches_switch_go_protocol_contract() {
        let success = br#"{"header":{"functionExecutionStatus":{"status":"Executed-Success"}}}"#;
        assert!(decode_success_response::<HeaderOnly>(success).is_ok());

        let warning =
            br#"{"header":{"functionExecutionStatus":{"status":"Executed-WithWarning"}}}"#;
        assert!(decode_success_response::<HeaderOnly>(warning).is_ok());

        let failed = br#"{"header":{"functionExecutionStatus":{"status":"Failed","statusCodeData":{"subjectCode":"8","reasonCode":"1","message":"declined"}}}}"#;
        assert!(matches!(
            decode_success_response::<HeaderOnly>(failed),
            Err(HttpError::Remote { status, message: Some(message), .. }) if status == "Failed" && message == "declined"
        ));
    }

    #[test]
    fn response_base64_allows_wrapping_whitespace_and_enforces_limits() {
        assert_eq!(DecodedB64("AQI=\r\n".into()).into_bytes(2).unwrap(), [1, 2]);
        assert!(matches!(
            DecodedB64("AQI=".into()).into_bytes(1),
            Err(HttpError::TooLarge)
        ));
        assert!(matches!(
            DecodedB64("@@".into()).into_bytes(8),
            Err(HttpError::Base64(_))
        ));
    }

    #[test]
    fn transaction_id_must_be_bounded_hex_and_stable() {
        assert_eq!(decode_transaction_id("00aF").unwrap(), [0, 0xaf]);
        assert!(ensure_same_server_transaction("00af", "00AF").is_ok());
        assert!(ensure_same_server_transaction("00af", "00a0").is_err());
        assert!(decode_transaction_id("abc").is_err());
        assert!(decode_transaction_id("zz").is_err());
        assert!(decode_transaction_id(&"a".repeat(130)).is_err());
    }

    #[test]
    fn endpoint_construction_rejects_non_https_and_authority_injection() {
        let code = ActivationCode::parse("LPA:1$example.invalid$matching").unwrap();
        assert_eq!(
            activation_endpoint(&code, "initiateAuthentication").unwrap(),
            "https://example.invalid/gsma/rsp2/es9plus/initiateAuthentication"
        );
        assert!(notification_endpoint("http://example.invalid").is_err());
        assert!(notification_endpoint("https://user@example.invalid").is_err());
        assert!(notification_endpoint("example.invalid/path").is_err());
        assert_eq!(
            notification_endpoint("example.invalid").unwrap(),
            "https://example.invalid/gsma/rsp2/es9plus/handleNotification"
        );
    }

    #[test]
    fn request_json_uses_go_admin_protocol_fields_and_standard_base64() {
        let request = InitiateRequest {
            challenge: B64(&[1, 2, 3]),
            euicc_info1: B64(&[4, 5]),
            smdp_address: "smdp.example",
        };
        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["euiccChallenge"], "AQID");
        assert_eq!(json["euiccInfo1"], "BAU=");
        assert_eq!(json["smdpAddress"], "smdp.example");
        assert_eq!(ADMIN_PROTOCOL, "gsma/rsp/v2.5.0");
    }

    #[test]
    fn local_http_fixture_checks_headers_json_status_and_response_bound() {
        let response = br#"{"header":{"functionExecutionStatus":{"status":"Executed-Success"}}}"#;
        let (url, server) = serve_once(200, response.to_vec());
        let request = InitiateRequest {
            challenge: B64(&[0x10, 0x20]),
            euicc_info1: B64(&[0x30, 0x40]),
            smdp_address: "smdp.example",
        };
        let _: HeaderOnly = test_client()
            .post_json(&url, &request, 1024, Duration::from_secs(2))
            .unwrap();
        let captured = server.join().unwrap();
        let header_end = captured
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let headers = std::str::from_utf8(&captured[..header_end])
            .unwrap()
            .to_ascii_lowercase();
        assert!(headers.contains("x-admin-protocol: gsma/rsp/v2.5.0"));
        assert!(headers.contains("user-agent: gsma-rsp-lpad"));
        let body: serde_json::Value = serde_json::from_slice(&captured[header_end + 4..]).unwrap();
        assert_eq!(body["euiccChallenge"], "ECA=");
        assert_eq!(body["euiccInfo1"], "MEA=");
        assert_eq!(body["smdpAddress"], "smdp.example");

        let status_request = InitiateRequest {
            challenge: B64(&[1]),
            euicc_info1: B64(&[2]),
            smdp_address: "smdp.example",
        };
        let (url, server) = serve_once(503, Vec::new());
        assert!(matches!(
            test_client().post_json::<_, HeaderOnly>(
                &url,
                &status_request,
                1024,
                Duration::from_secs(2)
            ),
            Err(HttpError::Status(503))
        ));
        let _ = server.join().unwrap();

        let oversized = br#"{"header":{"functionExecutionStatus":{"status":"Executed-Success"}},"padding":"large"}"#;
        let (url, server) = serve_once(200, oversized.to_vec());
        assert!(matches!(
            test_client().post_json::<_, HeaderOnly>(
                &url,
                &status_request,
                32,
                Duration::from_secs(2)
            ),
            Err(HttpError::TooLarge)
        ));
        let _ = server.join().unwrap();
    }
}
