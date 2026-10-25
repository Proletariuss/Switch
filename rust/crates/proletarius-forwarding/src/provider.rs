//! Rust provider HTTP delivery for SMS forwarding.
//!
//! A transport error after request submission is reported as unknown because
//! the remote service may have accepted the notification. Queue owners must
//! persist an explicit pre-submit checkpoint and only confirm after a strict
//! provider acknowledgement.

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client, Response, StatusCode, Url, redirect::Policy};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use zeroize::Zeroizing;

const MAX_RESPONSE_BYTES: usize = 32 * 1024;
const TELEGRAM_TIMEOUT: Duration = Duration::from_secs(20);
const BARK_TIMEOUT: Duration = Duration::from_secs(15);
pub const TELEGRAM_CHUNK_LIMIT: usize = 4000;

/// Proxy behavior for Telegram. System mode uses the host's configured HTTP
/// proxy environment/system-proxy integration; it never silently becomes a
/// configured manual proxy. Direct is an explicit bypass.
#[derive(Clone)]
pub enum ProxyMode {
    System,
    Direct,
    Manual(String),
}

impl std::fmt::Debug for ProxyMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System => f.write_str("System"),
            Self::Direct => f.write_str("Direct"),
            Self::Manual(_) => f.write_str("Manual([REDACTED])"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryDisposition {
    /// A known server rejection that should not be retried automatically.
    Permanent,
    /// A known server rejection or pre-submit failure that may be retried.
    Retryable,
    /// The request may have been accepted; retrying could duplicate a message.
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryFailure {
    pub code: String,
    pub disposition: DeliveryDisposition,
    pub retry_after: Option<Duration>,
}

impl DeliveryFailure {
    fn new(code: impl Into<String>, disposition: DeliveryDisposition) -> Self {
        Self {
            code: code.into(),
            disposition,
            retry_after: None,
        }
    }

    fn with_retry_after(mut self, retry_after: Option<Duration>) -> Self {
        self.retry_after = retry_after;
        self
    }
}

impl std::fmt::Display for DeliveryFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.code)
    }
}

impl std::error::Error for DeliveryFailure {}

/// Message fields rendered with the existing Go provider's SMS format.
/// `timestamp_local` is already rendered in the host's local time zone by the
/// forwarding runtime; an absent timestamp omits the time line.
pub struct ForwardedSms<'a> {
    pub sender: &'a str,
    pub device: &'a str,
    pub timestamp_local: Option<&'a str>,
    pub content: &'a str,
}

/// Render the existing SMS layout and split at Unicode scalar boundaries
/// using the Go forwarder's 4,000-character chunk budget.
pub fn telegram_chunks(message: &ForwardedSms<'_>) -> Vec<Zeroizing<String>> {
    split_telegram_text(&format_sms(message))
}

impl std::fmt::Debug for ForwardedSms<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ForwardedSms([REDACTED])")
    }
}

pub struct TelegramSender {
    // System proxy/PAC selection belongs to each submission, not to the
    // configuration transaction. A temporarily unavailable resolver must not
    // prevent the forwarding worker from being installed.
    client: Option<Client>,
    proxy: ProxyMode,
    system_proxy_resolver: Arc<SystemProxyResolver>,
    endpoint: Url,
    bot_token: Zeroizing<String>,
    chat_id: Zeroizing<String>,
}

impl TelegramSender {
    pub fn new(
        bot_token: impl Into<String>,
        chat_id: impl Into<String>,
        proxy: ProxyMode,
    ) -> Result<Self, DeliveryFailure> {
        let endpoint = Url::parse("https://api.telegram.org")
            .expect("Telegram provider origin is a valid URL");
        Self::with_endpoint(bot_token, chat_id, proxy, endpoint)
    }

    pub(crate) fn with_endpoint(
        bot_token: impl Into<String>,
        chat_id: impl Into<String>,
        proxy: ProxyMode,
        endpoint: Url,
    ) -> Result<Self, DeliveryFailure> {
        Self::with_endpoint_and_resolver(
            bot_token,
            chat_id,
            proxy,
            endpoint,
            proletarius_system_proxy::resolve_for_url,
        )
    }

    fn with_endpoint_and_resolver<F>(
        bot_token: impl Into<String>,
        chat_id: impl Into<String>,
        proxy: ProxyMode,
        endpoint: Url,
        resolver: F,
    ) -> Result<Self, DeliveryFailure>
    where
        F: Fn(
                &str,
            )
                -> Result<proletarius_system_proxy::Decision, proletarius_system_proxy::ResolveError>
            + Send
            + Sync
            + 'static,
    {
        let bot_token = Zeroizing::new(bot_token.into());
        let chat_id = Zeroizing::new(chat_id.into());
        if bot_token.trim().is_empty() || chat_id.trim().is_empty() {
            return Err(DeliveryFailure::new(
                "telegram_config_invalid",
                DeliveryDisposition::Permanent,
            ));
        }
        let resolver: Arc<SystemProxyResolver> = Arc::new(resolver);
        let client = if matches!(proxy, ProxyMode::System) {
            None
        } else {
            Some(build_client_for_target_with_resolver(
                TELEGRAM_TIMEOUT,
                Some(proxy.clone()),
                endpoint.as_str(),
                resolver.as_ref(),
            )?)
        };
        Ok(Self {
            client,
            proxy,
            system_proxy_resolver: resolver,
            endpoint,
            bot_token,
            chat_id,
        })
    }

    /// Send one Telegram text chunk. The queue owner is responsible for
    /// checkpointing the chunk before calling this and confirming it only on
    /// `Ok(())`.
    pub async fn send_chunk(&self, text: &str) -> Result<(), DeliveryFailure> {
        let mut endpoint = self.endpoint.clone();
        endpoint
            .path_segments_mut()
            .map_err(|_| {
                DeliveryFailure::new("telegram_request_invalid", DeliveryDisposition::Permanent)
            })?
            .pop_if_empty()
            .push(&format!("bot{}", self.bot_token.as_str()))
            .push("sendMessage");

        let payload = TelegramPayload {
            chat_id: self.chat_id.as_str(),
            text,
        };
        // The macOS proxy/PAC decision is not process-static: users can switch
        // Surge profiles or update System Configuration while Switch remains
        // open. Re-resolve it immediately before each HTTP submission so a
        // long-running forwarding worker doesn't keep using an obsolete
        // endpoint or accidentally fall back to the previous route.
        let refreshed_client;
        let client = if matches!(&self.proxy, ProxyMode::System) {
            refreshed_client = build_client_for_target_with_resolver(
                TELEGRAM_TIMEOUT,
                Some(ProxyMode::System),
                self.endpoint.as_str(),
                self.system_proxy_resolver.as_ref(),
            )?;
            &refreshed_client
        } else {
            self.client
                .as_ref()
                .expect("non-system Telegram proxy has a configured client")
        };
        let response = client
            .post(endpoint)
            .json(&payload)
            .send()
            .await
            .map_err(|error| submission_error("telegram", error))?;
        handle_telegram_response(response).await
    }
}

impl std::fmt::Debug for TelegramSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TelegramSender([REDACTED])")
    }
}

#[derive(Serialize)]
struct TelegramPayload<'a> {
    chat_id: &'a str,
    text: &'a str,
}

pub struct BarkSender {
    client: Client,
    server: Url,
    device_key: Zeroizing<String>,
    icon_url: Option<String>,
    group: Option<String>,
    encrypted: Option<(super::BarkEncryptionAlgorithm, Zeroizing<String>)>,
}

impl BarkSender {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        server_url: &str,
        device_key: impl Into<String>,
        icon_url: Option<String>,
        group: Option<String>,
        encryption: Option<(super::BarkEncryptionAlgorithm, String)>,
    ) -> Result<Self, DeliveryFailure> {
        Self::with_timeout(
            server_url,
            device_key,
            icon_url,
            group,
            encryption,
            BARK_TIMEOUT,
        )
    }

    pub(crate) fn with_timeout(
        server_url: &str,
        device_key: impl Into<String>,
        icon_url: Option<String>,
        group: Option<String>,
        encryption: Option<(super::BarkEncryptionAlgorithm, String)>,
        timeout: Duration,
    ) -> Result<Self, DeliveryFailure> {
        Self::with_timeout_and_proxy(
            server_url, device_key, icon_url, group, encryption, None, timeout,
        )
    }

    fn with_timeout_and_proxy(
        server_url: &str,
        device_key: impl Into<String>,
        icon_url: Option<String>,
        group: Option<String>,
        encryption: Option<(super::BarkEncryptionAlgorithm, String)>,
        proxy: Option<ProxyMode>,
        timeout: Duration,
    ) -> Result<Self, DeliveryFailure> {
        let server = Url::parse(server_url.trim()).map_err(|_| {
            DeliveryFailure::new("bark_config_invalid", DeliveryDisposition::Permanent)
        })?;
        let loopback_http = server.scheme() == "http"
            && server.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
            });
        if (server.scheme() != "https" && !loopback_http) || server.host_str().is_none() {
            return Err(DeliveryFailure::new(
                "bark_config_invalid",
                DeliveryDisposition::Permanent,
            ));
        }
        if let Some(icon) = icon_url.as_deref() {
            let icon_valid = Url::parse(icon.trim())
                .is_ok_and(|url| url.scheme() == "https" && url.host_str().is_some());
            if !icon_valid {
                return Err(DeliveryFailure::new(
                    "bark_config_invalid",
                    DeliveryDisposition::Permanent,
                ));
            }
        }
        let device_key = device_key.into();
        if device_key.trim().is_empty() {
            return Err(DeliveryFailure::new(
                "bark_config_invalid",
                DeliveryDisposition::Permanent,
            ));
        }
        let device_key = Zeroizing::new(device_key);
        let encrypted = encryption.map(|(algorithm, key)| (algorithm, Zeroizing::new(key)));
        if let Some((algorithm, key)) = &encrypted {
            let required = match algorithm {
                super::BarkEncryptionAlgorithm::Aes128Cbc => 16,
                super::BarkEncryptionAlgorithm::Aes256Cbc => 32,
            };
            if key.len() != required {
                return Err(DeliveryFailure::new(
                    "bark_config_invalid",
                    DeliveryDisposition::Permanent,
                ));
            }
        }
        let client = build_client(timeout, proxy)?;
        Ok(Self {
            client,
            server,
            device_key,
            icon_url,
            group,
            encrypted,
        })
    }

    pub async fn send(&self, message: &ForwardedSms<'_>) -> Result<(), DeliveryFailure> {
        let title = "Switch 短信";
        let body = format_sms(message);
        let response = if let Some((algorithm, key)) = &self.encrypted {
            let payload = BarkEncryptedPayload {
                title,
                body: &body,
                icon: self.icon_url.as_deref(),
                group: self.group.as_deref(),
            };
            let plaintext = Zeroizing::new(serde_json::to_vec(&payload).map_err(|_| {
                DeliveryFailure::new("bark_payload_encode_failed", DeliveryDisposition::Retryable)
            })?);
            let (ciphertext, iv) = super::encrypt_bark_payload(
                &plaintext,
                *algorithm,
                key.as_bytes(),
            )
            .map_err(|_| {
                DeliveryFailure::new("bark_encryption_failed", DeliveryDisposition::Retryable)
            })?;
            let mut endpoint = self.server.clone();
            endpoint
                .path_segments_mut()
                .map_err(|_| {
                    DeliveryFailure::new("bark_request_invalid", DeliveryDisposition::Permanent)
                })?
                .pop_if_empty()
                .push(self.device_key.as_str());
            let parameters = [("ciphertext", ciphertext), ("iv", iv)];
            self.client.post(endpoint).form(&parameters).send().await
        } else {
            let mut endpoint = self.server.clone();
            endpoint
                .path_segments_mut()
                .map_err(|_| {
                    DeliveryFailure::new("bark_request_invalid", DeliveryDisposition::Permanent)
                })?
                .pop_if_empty()
                .push("push");
            let payload = BarkPlainPayload {
                device_key: self.device_key.as_str(),
                title,
                body: &body,
                icon: self.icon_url.as_deref(),
                group: self.group.as_deref(),
            };
            self.client.post(endpoint).json(&payload).send().await
        }
        .map_err(|error| submission_error("bark", error))?;
        handle_bark_response(response).await
    }
}

impl std::fmt::Debug for BarkSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BarkSender([REDACTED])")
    }
}

#[derive(Serialize)]
struct BarkPlainPayload<'a> {
    device_key: &'a str,
    title: &'a str,
    body: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<&'a str>,
}

#[derive(Serialize)]
struct BarkEncryptedPayload<'a> {
    title: &'a str,
    body: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<&'a str>,
}

type SystemProxyResolver = dyn Fn(&str) -> Result<proletarius_system_proxy::Decision, proletarius_system_proxy::ResolveError>
    + Send
    + Sync;

fn build_client(timeout: Duration, proxy: Option<ProxyMode>) -> Result<Client, DeliveryFailure> {
    build_client_for_target(timeout, proxy, "https://api.telegram.org")
}

fn build_client_for_target(
    timeout: Duration,
    proxy: Option<ProxyMode>,
    target: &str,
) -> Result<Client, DeliveryFailure> {
    build_client_for_target_with_resolver(
        timeout,
        proxy,
        target,
        &proletarius_system_proxy::resolve_for_url,
    )
}

fn build_client_for_target_with_resolver(
    timeout: Duration,
    proxy: Option<ProxyMode>,
    target: &str,
    resolver: &SystemProxyResolver,
) -> Result<Client, DeliveryFailure> {
    let mut builder = Client::builder().timeout(timeout).redirect(Policy::none());
    match proxy {
        Some(ProxyMode::System) => {
            let decision = resolver(target).map_err(|failure| {
                let disposition = match failure {
                    proletarius_system_proxy::ResolveError::NotFound
                    | proletarius_system_proxy::ResolveError::NativeResolutionFailed => {
                        DeliveryDisposition::Retryable
                    }
                    proletarius_system_proxy::ResolveError::InvalidConfiguration => {
                        DeliveryDisposition::Permanent
                    }
                };
                DeliveryFailure::new(failure.to_string(), disposition)
            })?;
            builder = match decision {
                // System mode is a proxy-only forwarding policy. A PAC DIRECT
                // result must not bypass the user's configured proxy path.
                proletarius_system_proxy::Decision::Direct => {
                    return Err(DeliveryFailure::new(
                        "telegram_proxy_policy_rejected",
                        DeliveryDisposition::Retryable,
                    ));
                }
                proletarius_system_proxy::Decision::Proxy(proxy) => {
                    builder
                        .no_proxy()
                        .proxy(reqwest::Proxy::all(proxy).map_err(|_| {
                            DeliveryFailure::new(
                                "telegram_proxy_invalid",
                                DeliveryDisposition::Permanent,
                            )
                        })?)
                }
            };
        }
        None => {}
        Some(ProxyMode::Direct) => builder = builder.no_proxy(),
        Some(ProxyMode::Manual(value)) => {
            let parsed = manual_proxy_url(&value)?;
            builder = builder
                .no_proxy()
                .proxy(reqwest::Proxy::all(parsed).map_err(|_| {
                    DeliveryFailure::new("telegram_proxy_invalid", DeliveryDisposition::Permanent)
                })?);
        }
    }
    builder.build().map_err(|_| {
        DeliveryFailure::new("http_client_unavailable", DeliveryDisposition::Retryable)
    })
}

fn manual_proxy_url(value: &str) -> Result<Url, DeliveryFailure> {
    let invalid = || DeliveryFailure::new("telegram_proxy_invalid", DeliveryDisposition::Permanent);
    let value = value.trim();
    let mut parsed = Url::parse(value).map_err(|_| invalid())?;
    if parsed.host_str().is_none()
        || !has_explicit_port(value)
        || !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h")
        || (matches!(parsed.scheme(), "socks5" | "socks5h")
            && parsed.path() != ""
            && parsed.path() != "/")
    {
        return Err(invalid());
    }
    if parsed.scheme() == "socks5" {
        // Existing saved SOCKS5 URLs keep their host, port and credentials,
        // but Telegram's hostname must be resolved by the proxy, not the Mac.
        parsed.set_scheme("socks5h").map_err(|_| invalid())?;
    }
    Ok(parsed)
}

fn has_explicit_port(value: &str) -> bool {
    let Some((_, remainder)) = value.split_once("://") else {
        return false;
    };
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    let host_and_port = authority.rsplit('@').next().unwrap_or_default();
    host_and_port
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
}

fn format_sms(message: &ForwardedSms<'_>) -> String {
    let mut lines = Vec::with_capacity(5);
    if !message.sender.trim().is_empty() {
        lines.push(format!("发件人：{}", message.sender));
    }
    if !message.device.trim().is_empty() {
        lines.push(format!("线路：{}", message.device));
    }
    if let Some(timestamp) = message.timestamp_local.filter(|value| !value.is_empty()) {
        lines.push(format!("时间：{timestamp}"));
    }
    lines.push(String::new());
    lines.push(message.content.to_owned());
    lines.join("\n")
}

fn split_telegram_text(text: &str) -> Vec<Zeroizing<String>> {
    let capacity = text.chars().count().div_ceil(TELEGRAM_CHUNK_LIMIT);
    let mut chunks = Vec::with_capacity(capacity.max(1));
    let mut current = String::new();
    let mut current_chars = 0;
    for character in text.chars() {
        if current_chars == TELEGRAM_CHUNK_LIMIT {
            chunks.push(Zeroizing::new(std::mem::take(&mut current)));
            current_chars = 0;
        }
        current.push(character);
        current_chars += 1;
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(Zeroizing::new(current));
    }
    chunks
}

async fn handle_telegram_response(response: Response) -> Result<(), DeliveryFailure> {
    let status = response.status();
    if !status.is_success() {
        let mut failure = http_failure("telegram", status, response.headers(), None);
        if failure.retry_after.is_none()
            && let Ok(bytes) = read_limited(response, "telegram").await
            && let Ok(acknowledgement) = serde_json::from_slice::<TelegramAcknowledgement>(&bytes)
        {
            failure.retry_after = acknowledgement
                .retry_after
                .filter(|seconds| *seconds > 0)
                .map(Duration::from_secs);
        }
        return Err(failure);
    }
    let bytes = read_limited(response, "telegram").await?;
    let acknowledgement: TelegramAcknowledgement =
        serde_json::from_slice(&bytes).map_err(|_| {
            DeliveryFailure::new("telegram_response_invalid", DeliveryDisposition::Unknown)
        })?;
    match acknowledgement.ok {
        Some(true) => Ok(()),
        Some(false) => {
            let mut failure = DeliveryFailure::new(
                "telegram_api_rejected",
                if acknowledgement
                    .error_code
                    .is_some_and(permanent_http_failure)
                {
                    DeliveryDisposition::Permanent
                } else {
                    DeliveryDisposition::Retryable
                },
            );
            failure.retry_after = acknowledgement
                .retry_after
                .filter(|seconds| *seconds > 0)
                .map(Duration::from_secs);
            Err(failure)
        }
        None => Err(DeliveryFailure::new(
            "telegram_response_invalid",
            DeliveryDisposition::Unknown,
        )),
    }
}

async fn handle_bark_response(response: Response) -> Result<(), DeliveryFailure> {
    let status = response.status();
    if !status.is_success() {
        return Err(http_failure("bark", status, response.headers(), None));
    }
    let bytes = read_limited(response, "bark").await?;
    let acknowledgement: BarkAcknowledgement = serde_json::from_slice(&bytes)
        .map_err(|_| DeliveryFailure::new("bark_response_invalid", DeliveryDisposition::Unknown))?;
    match acknowledgement.code {
        Some(StatusCode::OK) => Ok(()),
        Some(code) if (400..=599).contains(&code.as_u16()) => {
            let code_number = code.as_u16();
            Err(DeliveryFailure::new(
                format!("bark_api_{code_number}"),
                if permanent_http_failure(code_number) {
                    DeliveryDisposition::Permanent
                } else {
                    DeliveryDisposition::Retryable
                },
            ))
        }
        _ => Err(DeliveryFailure::new(
            "bark_response_invalid",
            DeliveryDisposition::Unknown,
        )),
    }
}

async fn read_limited(mut response: Response, provider: &str) -> Result<Vec<u8>, DeliveryFailure> {
    let mut body = Vec::with_capacity(512);
    loop {
        let chunk = response.chunk().await.map_err(|_| {
            DeliveryFailure::new(
                format!("{provider}_response_read_failed"),
                DeliveryDisposition::Unknown,
            )
        })?;
        let Some(chunk) = chunk else { break };
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(DeliveryFailure::new(
                format!("{provider}_response_too_large"),
                DeliveryDisposition::Unknown,
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn http_failure(
    provider: &str,
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    body_retry_after: Option<Duration>,
) -> DeliveryFailure {
    let retry_after = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .or_else(|| {
            headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| httpdate::parse_http_date(value).ok())
                .and_then(|time| time.duration_since(std::time::SystemTime::now()).ok())
        })
        .or(body_retry_after);
    DeliveryFailure::new(
        format!("{provider}_http_{}", status.as_u16()),
        if permanent_http_failure(status.as_u16()) {
            DeliveryDisposition::Permanent
        } else if status == StatusCode::TOO_MANY_REQUESTS {
            DeliveryDisposition::Retryable
        } else {
            // A provider response proves the POST reached an HTTP server, but
            // a 5xx/408 response does not prove the side effect was rejected.
            DeliveryDisposition::Unknown
        },
    )
    .with_retry_after(retry_after)
}

fn submission_error(provider: &str, error: reqwest::Error) -> DeliveryFailure {
    // Only errors reqwest identifies as connection establishment failures are
    // safe to retry. Hyper currently drops the status line from failed HTTPS
    // CONNECT responses, so proxy auth/policy and temporary gateway failures
    // share this bounded retry path. A timeout/reset after POST submission
    // remains Unknown.
    if !error.is_connect() {
        return DeliveryFailure::new(
            format!("{provider}_network_error"),
            DeliveryDisposition::Unknown,
        );
    }
    match error.status() {
        Some(StatusCode::PROXY_AUTHENTICATION_REQUIRED) => DeliveryFailure::new(
            format!("{provider}_proxy_auth_required"),
            DeliveryDisposition::Permanent,
        ),
        Some(StatusCode::FORBIDDEN) => DeliveryFailure::new(
            format!("{provider}_proxy_policy_rejected"),
            DeliveryDisposition::Permanent,
        ),
        Some(StatusCode::BAD_REQUEST) | Some(StatusCode::NOT_FOUND) => DeliveryFailure::new(
            format!("{provider}_proxy_configuration_rejected"),
            DeliveryDisposition::Permanent,
        ),
        Some(StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS) => DeliveryFailure::new(
            format!("{provider}_proxy_unavailable"),
            DeliveryDisposition::Retryable,
        ),
        Some(status) if status.is_server_error() => DeliveryFailure::new(
            format!("{provider}_proxy_unavailable"),
            DeliveryDisposition::Retryable,
        ),
        Some(status) if status.is_client_error() => DeliveryFailure::new(
            format!("{provider}_proxy_configuration_rejected"),
            DeliveryDisposition::Permanent,
        ),
        _ => DeliveryFailure::new(
            format!("{provider}_connect_failed"),
            DeliveryDisposition::Retryable,
        ),
    }
}

fn permanent_http_failure(status: u16) -> bool {
    (300..500).contains(&status) && status != 408 && status != 429
}

#[derive(Default)]
struct TelegramAcknowledgement {
    ok: Option<bool>,
    error_code: Option<u16>,
    retry_after: Option<u64>,
}

impl<'de> Deserialize<'de> for TelegramAcknowledgement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AcknowledgementVisitor;

        impl<'de> Visitor<'de> for AcknowledgementVisitor {
            type Value = TelegramAcknowledgement;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Telegram response object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut response = TelegramAcknowledgement::default();
                let mut saw_ok = false;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "ok" => {
                            if saw_ok {
                                return Err(serde::de::Error::custom("duplicate ok field"));
                            }
                            saw_ok = true;
                            response.ok = map.next_value::<Option<bool>>()?;
                        }
                        "error_code" => response.error_code = map.next_value::<Option<u16>>()?,
                        "parameters" => {
                            response.retry_after = map
                                .next_value::<Option<TelegramParameters>>()?
                                .and_then(|parameters| parameters.retry_after);
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(response)
            }
        }

        deserializer.deserialize_map(AcknowledgementVisitor)
    }
}

#[derive(Deserialize)]
struct TelegramParameters {
    retry_after: Option<u64>,
}

#[derive(Default)]
struct BarkAcknowledgement {
    code: Option<StatusCode>,
}

impl<'de> Deserialize<'de> for BarkAcknowledgement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BarkAcknowledgementVisitor;

        impl<'de> Visitor<'de> for BarkAcknowledgementVisitor {
            type Value = BarkAcknowledgement;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Bark acknowledgement object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut response = BarkAcknowledgement::default();
                let mut saw_code = false;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "code" {
                        if saw_code {
                            return Err(serde::de::Error::custom("duplicate code field"));
                        }
                        saw_code = true;
                        let number = map.next_value::<Option<u16>>()?;
                        response.code = number.and_then(|value| StatusCode::from_u16(value).ok());
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(response)
            }
        }

        deserializer.deserialize_map(BarkAcknowledgementVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    async fn mock_response(
        status: &str,
        body: &str,
        extra_headers: &str,
    ) -> (Url, oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = oneshot::channel();
        let body = body.to_owned();
        let status = status.to_owned();
        let extra_headers = extra_headers.to_owned();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 2048];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let headers_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| index + 4)
                .unwrap_or(request.len());
            let headers = String::from_utf8_lossy(&request[..headers_end]).to_string();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while request.len().saturating_sub(headers_end) < content_length {
                let count = stream.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            let captured = String::from_utf8_lossy(&request).to_string();
            let _ = sender.send(captured);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        (Url::parse(&format!("http://{address}")).unwrap(), receiver)
    }

    async fn mock_connect_proxy() -> (String, oneshot::Receiver<String>) {
        mock_connect_proxy_status("502 Synthetic Proxy", "").await
    }

    async fn mock_connect_proxy_status(
        status: &str,
        extra_headers: &str,
    ) -> (String, oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = oneshot::channel();
        let status = status.to_owned();
        let extra_headers = extra_headers.to_owned();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let first_line = String::from_utf8_lossy(&request)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            let _ = sender.send(first_line);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n{extra_headers}\r\n"
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        (address.to_string(), receiver)
    }

    fn sms<'a>() -> ForwardedSms<'a> {
        ForwardedSms {
            sender: "65075",
            device: "读卡器",
            timestamp_local: Some("2026-10-04 09:30:00"),
            content: "PAC",
        }
    }

    #[tokio::test]
    async fn telegram_posts_formatted_chunk_and_requires_explicit_ack() {
        let (endpoint, request) = mock_response(
            "200 OK",
            r#"{"ok":true,"result":{"message_id":1}}"#,
            "Content-Type: application/json\r\n",
        )
        .await;
        let sender =
            TelegramSender::with_endpoint("123:secret", "chat-1", ProxyMode::Direct, endpoint)
                .unwrap();
        sender.send_chunk(&format_sms(&sms())).await.unwrap();
        let request = request.await.unwrap();
        assert!(request.starts_with("POST /bot123:secret/sendMessage HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/json")
        );
        assert!(request.contains(r#""chat_id":"chat-1""#));
        assert!(
            request.contains("发件人：65075\\n线路：读卡器\\n时间：2026-10-04 09:30:00\\n\\nPAC")
        );
        assert!(!format!("{sender:?}").contains("secret"));
    }

    #[tokio::test]
    async fn telegram_system_proxy_is_resolved_again_for_each_submission() {
        let (first_proxy, first_request) = mock_connect_proxy().await;
        let (next_proxy, next_request) = mock_connect_proxy().await;
        let resolution = Arc::new(AtomicUsize::new(0));
        let test_resolution = resolution.clone();
        let first_proxy = Url::parse(&format!("http://{first_proxy}")).unwrap();
        let next_proxy = Url::parse(&format!("http://{next_proxy}")).unwrap();
        let sender = TelegramSender::with_endpoint_and_resolver(
            "synthetic-token",
            "synthetic-chat",
            ProxyMode::System,
            Url::parse("https://api.telegram.org").unwrap(),
            move |_| {
                if test_resolution.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(proletarius_system_proxy::Decision::Proxy(
                        first_proxy.clone(),
                    ))
                } else {
                    Ok(proletarius_system_proxy::Decision::Proxy(
                        next_proxy.clone(),
                    ))
                }
            },
        )
        .expect("initial system proxy is valid");

        assert_eq!(resolution.load(Ordering::SeqCst), 0);

        let failure = sender
            .send_chunk("first local proxy route fixture")
            .await
            .expect_err("synthetic proxy rejects the request before TLS");
        assert_eq!(failure.code, "telegram_connect_failed");
        assert_eq!(failure.disposition, DeliveryDisposition::Retryable);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), first_request)
                .await
                .expect("first proxy received the request")
                .unwrap(),
            "CONNECT api.telegram.org:443 HTTP/1.1"
        );

        let failure = sender
            .send_chunk("next local proxy route fixture")
            .await
            .expect_err("synthetic proxy rejects the request before TLS");
        assert_eq!(failure.code, "telegram_connect_failed");
        assert_eq!(failure.disposition, DeliveryDisposition::Retryable);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), next_request)
                .await
                .expect("refreshed proxy received the request")
                .unwrap(),
            "CONNECT api.telegram.org:443 HTTP/1.1"
        );
        assert_eq!(resolution.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn telegram_system_proxy_recovers_after_temporary_resolution_failure() {
        let (proxy, request) = mock_connect_proxy().await;
        let proxy = Url::parse(&format!("http://{proxy}")).unwrap();
        let resolution = Arc::new(AtomicUsize::new(0));
        let test_resolution = resolution.clone();
        let sender = TelegramSender::with_endpoint_and_resolver(
            "synthetic-token",
            "synthetic-chat",
            ProxyMode::System,
            Url::parse("https://api.telegram.org").unwrap(),
            move |_| {
                if test_resolution.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(proletarius_system_proxy::ResolveError::NotFound)
                } else {
                    Ok(proletarius_system_proxy::Decision::Proxy(proxy.clone()))
                }
            },
        )
        .expect("temporary system proxy outage must not reject configuration");

        let failure = sender.send_chunk("first attempt").await.unwrap_err();
        assert_eq!(failure.code, "system_proxy_not_found");
        assert_eq!(failure.disposition, DeliveryDisposition::Retryable);

        let failure = sender.send_chunk("after proxy returns").await.unwrap_err();
        assert_eq!(failure.code, "telegram_connect_failed");
        assert_eq!(failure.disposition, DeliveryDisposition::Retryable);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), request)
                .await
                .expect("restored proxy received the request")
                .unwrap(),
            "CONNECT api.telegram.org:443 HTTP/1.1"
        );
        assert_eq!(resolution.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn telegram_system_proxy_direct_decision_never_submits_directly() {
        let sender = TelegramSender::with_endpoint_and_resolver(
            "synthetic-token",
            "synthetic-chat",
            ProxyMode::System,
            Url::parse("https://api.telegram.org").unwrap(),
            |_| Ok(proletarius_system_proxy::Decision::Direct),
        )
        .unwrap();
        let failure = sender
            .send_chunk("proxy policy is direct")
            .await
            .unwrap_err();
        assert_eq!(failure.code, "telegram_proxy_policy_rejected");
        assert_eq!(failure.disposition, DeliveryDisposition::Retryable);
    }

    #[tokio::test]
    async fn telegram_duplicate_ack_is_unknown_and_429_preserves_retry_delay() {
        let (endpoint, _) = mock_response("200 OK", r#"{"ok":true,"ok":true}"#, "").await;
        let sender =
            TelegramSender::with_endpoint("token", "chat", ProxyMode::Direct, endpoint).unwrap();
        let error = sender.send_chunk("hello").await.unwrap_err();
        assert_eq!(error.code, "telegram_response_invalid");
        assert_eq!(error.disposition, DeliveryDisposition::Unknown);

        let (endpoint, _) = mock_response(
            "429 Too Many Requests",
            r#"{"ok":false,"parameters":{"retry_after":19}}"#,
            "Retry-After: 17\r\n",
        )
        .await;
        let sender =
            TelegramSender::with_endpoint("token", "chat", ProxyMode::Direct, endpoint).unwrap();
        let error = sender.send_chunk("hello").await.unwrap_err();
        assert_eq!(error.code, "telegram_http_429");
        assert_eq!(error.disposition, DeliveryDisposition::Retryable);
        assert_eq!(error.retry_after, Some(Duration::from_secs(17)));

        let (endpoint, _) = mock_response(
            "429 Too Many Requests",
            r#"{"ok":false,"parameters":{"retry_after":19}}"#,
            "",
        )
        .await;
        let sender =
            TelegramSender::with_endpoint("token", "chat", ProxyMode::Direct, endpoint).unwrap();
        let error = sender.send_chunk("hello").await.unwrap_err();
        assert_eq!(error.retry_after, Some(Duration::from_secs(19)));

        let (endpoint, _) = mock_response(
            "429 Too Many Requests",
            "ignored",
            "Retry-After: Thu, 01 Jan 2099 00:00:00 GMT\r\n",
        )
        .await;
        let sender =
            TelegramSender::with_endpoint("token", "chat", ProxyMode::Direct, endpoint).unwrap();
        let error = sender.send_chunk("hello").await.unwrap_err();
        assert!(error.retry_after.is_some_and(|delay| !delay.is_zero()));
    }

    #[tokio::test]
    async fn telegram_proxy_failures_are_retried_only_before_provider_submission() {
        for status in [
            "502 Bad Gateway",
            "503 Service Unavailable",
            "407 Proxy Authentication Required",
            "403 Forbidden",
        ] {
            let (proxy, connect_request) = mock_connect_proxy_status(status, "").await;
            let sender = TelegramSender::with_endpoint(
                "synthetic-token",
                "synthetic-chat",
                ProxyMode::Manual(format!("http://{proxy}")),
                Url::parse("https://api.telegram.org").unwrap(),
            )
            .unwrap();
            let failure = sender
                .send_chunk("pre-submit proxy failure")
                .await
                .expect_err("CONNECT failure must not be mistaken for delivery");
            assert_eq!(
                failure.code, "telegram_connect_failed",
                "proxy status {status}"
            );
            assert_eq!(
                failure.disposition,
                DeliveryDisposition::Retryable,
                "proxy status {status}"
            );
            assert_eq!(
                connect_request.await.unwrap(),
                "CONNECT api.telegram.org:443 HTTP/1.1"
            );
        }
    }

    #[tokio::test]
    async fn refused_connection_retries_but_post_submission_server_errors_are_unknown() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refused = listener.local_addr().unwrap();
        drop(listener);
        let sender = TelegramSender::with_endpoint(
            "synthetic-token",
            "synthetic-chat",
            ProxyMode::Direct,
            Url::parse(&format!("http://{refused}")).unwrap(),
        )
        .unwrap();
        let failure = sender.send_chunk("not submitted").await.unwrap_err();
        assert_eq!(failure.code, "telegram_connect_failed");
        assert_eq!(failure.disposition, DeliveryDisposition::Retryable);

        for status in ["502 Bad Gateway", "503 Service Unavailable"] {
            let (endpoint, request) = mock_response(status, "", "").await;
            let sender = TelegramSender::with_endpoint(
                "synthetic-token",
                "synthetic-chat",
                ProxyMode::Direct,
                endpoint,
            )
            .unwrap();
            let failure = sender
                .send_chunk("provider received the POST")
                .await
                .unwrap_err();
            assert_eq!(failure.disposition, DeliveryDisposition::Unknown);
            assert!(request.await.unwrap().starts_with("POST /bot"));
        }
    }

    #[tokio::test]
    async fn bark_refused_local_connection_is_a_safe_pre_submit_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refused = listener.local_addr().unwrap();
        drop(listener);
        let sender = BarkSender::with_timeout_and_proxy(
            &format!("http://{refused}"),
            "device",
            None,
            None,
            None,
            Some(ProxyMode::Direct),
            Duration::from_secs(1),
        )
        .unwrap();
        let failure = sender.send(&sms()).await.unwrap_err();
        assert_eq!(failure.code, "bark_connect_failed");
        assert_eq!(failure.disposition, DeliveryDisposition::Retryable);
    }

    #[tokio::test]
    async fn telegram_redirect_is_not_followed_and_oversized_ack_is_unknown() {
        let (endpoint, _) = mock_response(
            "302 Found",
            "",
            "Location: http://127.0.0.1:9/redirected\r\n",
        )
        .await;
        let sender =
            TelegramSender::with_endpoint("token", "chat", ProxyMode::Direct, endpoint).unwrap();
        let error = sender.send_chunk("hello").await.unwrap_err();
        assert_eq!(error.code, "telegram_http_302");
        assert_eq!(error.disposition, DeliveryDisposition::Permanent);

        let oversized = "x".repeat(MAX_RESPONSE_BYTES + 1);
        let (endpoint, _) = mock_response("200 OK", &oversized, "").await;
        let sender =
            TelegramSender::with_endpoint("token", "chat", ProxyMode::Direct, endpoint).unwrap();
        let error = sender.send_chunk("hello").await.unwrap_err();
        assert_eq!(error.code, "telegram_response_too_large");
        assert_eq!(error.disposition, DeliveryDisposition::Unknown);
    }

    #[tokio::test]
    async fn bark_plain_and_encrypted_requests_keep_contract_fields() {
        let (endpoint, request) = mock_response("200 OK", r#"{"code":200}"#, "").await;
        let server = format!("{}prefix", endpoint);
        let sender = BarkSender::with_timeout(
            &server,
            "device/key",
            Some("https://example.com/icon.png".to_owned()),
            Some("Switch-SMS".to_owned()),
            None,
            Duration::from_secs(1),
        )
        .unwrap();
        sender.send(&sms()).await.unwrap();
        let request = request.await.unwrap();
        assert!(
            request.starts_with("POST /prefix/push HTTP/1.1"),
            "unexpected Bark request line: {}",
            request.lines().next().unwrap_or_default()
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/json")
        );
        assert!(request.contains("device/key"));
        assert!(request.contains("https://example.com/icon.png"));

        let (endpoint, request) = mock_response("200 OK", r#"{"code":200}"#, "").await;
        let sender = BarkSender::with_timeout(
            endpoint.as_str(),
            "device/key",
            None,
            None,
            Some((
                super::super::BarkEncryptionAlgorithm::Aes128Cbc,
                "0123456789abcdef".into(),
            )),
            Duration::from_secs(1),
        )
        .unwrap();
        sender.send(&sms()).await.unwrap();
        let request = request.await.unwrap();
        assert!(request.starts_with("POST /device%2Fkey HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/x-www-form-urlencoded")
        );
        assert!(request.contains("ciphertext="));
        assert!(request.contains("iv="));
    }

    #[tokio::test]
    async fn bark_requires_single_integer_acknowledgement() {
        let (endpoint, _) = mock_response("200 OK", r#"{"code":200,"code":200}"#, "").await;
        let sender = BarkSender::with_timeout(
            endpoint.as_str(),
            "device",
            None,
            None,
            None,
            Duration::from_secs(1),
        )
        .unwrap();
        let error = sender.send(&sms()).await.unwrap_err();
        assert_eq!(error.code, "bark_response_invalid");
        assert_eq!(error.disposition, DeliveryDisposition::Unknown);

        let (endpoint, _) = mock_response("200 OK", r#"{"code":429}"#, "").await;
        let sender = BarkSender::with_timeout(
            endpoint.as_str(),
            "device",
            None,
            None,
            None,
            Duration::from_secs(1),
        )
        .unwrap();
        let error = sender.send(&sms()).await.unwrap_err();
        assert_eq!(error.code, "bark_api_429");
        assert_eq!(error.disposition, DeliveryDisposition::Retryable);
    }

    #[test]
    fn bark_rejects_remote_http_and_non_https_icon_configuration() {
        for (server, icon) in [
            ("http://bark.example", None),
            (
                "https://bark.example",
                Some("http://icons.example/icon.png".to_owned()),
            ),
        ] {
            assert_eq!(
                BarkSender::with_timeout(
                    server,
                    "device",
                    icon,
                    None,
                    None,
                    Duration::from_secs(1)
                )
                .unwrap_err()
                .code,
                "bark_config_invalid"
            );
        }
    }

    #[test]
    fn proxy_debug_and_provider_message_debug_redact_sensitive_values() {
        assert!(
            !format!(
                "{:?}",
                ProxyMode::Manual("http://user:password@host:8080".into())
            )
            .contains("password")
        );
        assert!(!format!("{:?}", sms()).contains("65075"));
    }

    #[test]
    fn manual_proxy_accepts_authenticated_http_and_socks_routes_only() {
        for proxy in [
            "http://user:password@proxy.example:8080",
            "http://proxy.example:80",
            "https://proxy.example:8443",
            "socks5://user:password@proxy.example:1080",
            "socks5h://proxy.example:1080",
            "socks5://[::1]:1080",
        ] {
            assert!(
                build_client(
                    Duration::from_secs(1),
                    Some(ProxyMode::Manual(proxy.to_owned()))
                )
                .is_ok(),
                "proxy URL should be accepted"
            );
        }
        for proxy in [
            "ftp://proxy.example:21",
            "http://proxy.example",
            "socks5://proxy.example:1080/path",
        ] {
            assert_eq!(
                build_client(
                    Duration::from_secs(1),
                    Some(ProxyMode::Manual(proxy.to_owned()))
                )
                .unwrap_err()
                .code,
                "telegram_proxy_invalid"
            );
        }
    }

    #[test]
    fn manual_socks5_resolves_telegram_hostname_at_proxy_without_losing_credentials() {
        let proxy = manual_proxy_url("  socks5://user:password@proxy.example:1080  ").unwrap();
        assert_eq!(proxy.scheme(), "socks5h");
        assert_eq!(proxy.username(), "user");
        assert_eq!(proxy.password(), Some("password"));
        assert_eq!(proxy.host_str(), Some("proxy.example"));
        assert_eq!(proxy.port(), Some(1080));
        assert_eq!(
            manual_proxy_url("socks5h://proxy.example:1080")
                .unwrap()
                .scheme(),
            "socks5h"
        );
        assert_eq!(
            manual_proxy_url("http://proxy.example:8080")
                .unwrap()
                .scheme(),
            "http"
        );
    }

    #[test]
    fn telegram_chunks_match_go_rune_boundaries_and_preserve_unicode() {
        let text = format!("{}🙂", "甲".repeat(TELEGRAM_CHUNK_LIMIT));
        let chunks = split_telegram_text(&text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), TELEGRAM_CHUNK_LIMIT);
        assert_eq!(chunks[1].as_str(), "🙂");
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= TELEGRAM_CHUNK_LIMIT)
        );
    }
}
