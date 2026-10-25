//! Rust EAP-AKA challenge response backed by the selected native PC/SC SIM.

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use proletarius_ike::eap_aka::AkaCheckcodeMode;
use proletarius_reader::{AkaApplication, AuthenticationError};
use proletarius_sim::AkaResult;
use proletarius_vowifi::{
    EapAkaExchangeError, EapAkaExchangeResult, Ikev2EapConversation, SimAkaResult,
};
use serde::Deserialize;
use serde_json::json;
use zeroize::Zeroizing;

// Keep this below Go's nativeReaderEAPAKAHTTPTimeout.  The remaining margin
// is for Unix-socket scheduling and serialisation of an explicit timeout
// response, preventing an ambiguous client timeout after a physical APDU has
// already completed.
const READER_EAP_AKA_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Deserialize)]
struct EapAkaRequest {
    reader: String,
    #[serde(default)]
    pcsc: Option<crate::reader_pcsc::Binding>,
    #[serde(default)]
    transaction: Option<crate::reader_reservation::CardTransaction>,
    application: String,
    exchange_id: String,
    #[serde(default)]
    expected_identity: Option<crate::reader_auth::ExpectedIdentity>,
    challenge: Vec<u8>,
    #[serde(default = "default_checkcode_mode")]
    checkcode_mode: String,
}

fn default_checkcode_mode() -> String {
    "minimal".to_owned()
}

struct EapAkaResponse {
    outcome: &'static str,
    packet: Zeroizing<Vec<u8>>,
    msk: Zeroizing<Vec<u8>>,
    k_aut: Zeroizing<Vec<u8>>,
}

#[derive(Debug)]
enum EapAkaRequestError {
    InvalidInput,
    Eap,
    Authentication(AuthenticationError),
    AuthenticationRejected,
    Owner(crate::reader_pcsc::Failure),
}

fn process<F>(
    input: EapAkaRequest,
    identity_transcript: &[u8],
    authenticate: F,
) -> Result<EapAkaResponse, EapAkaRequestError>
where
    F: FnOnce(&str, AkaApplication, &[u8; 16], &[u8; 16]) -> Result<AkaResult, AuthenticationError>,
{
    let reader = input.reader.trim();
    if reader.is_empty() || input.challenge.len() > 4_096 {
        return Err(EapAkaRequestError::InvalidInput);
    }
    let application = match input.application.as_str() {
        "usim" => AkaApplication::Usim,
        "isim" => AkaApplication::Isim,
        _ => return Err(EapAkaRequestError::InvalidInput),
    };
    let checkcode_mode = match input.checkcode_mode.as_str() {
        "minimal" => AkaCheckcodeMode::Minimal,
        "omit" => AkaCheckcodeMode::Omit,
        "checkcode" => AkaCheckcodeMode::Checkcode,
        "recompute" => AkaCheckcodeMode::Recompute,
        _ => return Err(EapAkaRequestError::InvalidInput),
    };
    let conversation =
        Ikev2EapConversation::from_identity_transcript(identity_transcript, checkcode_mode)
            .map_err(|_| EapAkaRequestError::InvalidInput)?;

    let result = conversation.answer_challenge_with_sim(&input.challenge, |rand, autn| {
        authenticate(reader, application, rand, autn).map(|result| match result {
            AkaResult::Success { res, ck, ik } => SimAkaResult::Success { res, ck, ik },
            AkaResult::SynchronizationFailure { auts } => {
                SimAkaResult::SynchronizationFailure { auts }
            }
            AkaResult::AuthenticationReject => SimAkaResult::AuthenticationRejected,
        })
    });
    match result {
        Ok(EapAkaExchangeResult::Challenge(response)) => Ok(EapAkaResponse {
            outcome: "success",
            packet: Zeroizing::new(response.packet.clone()),
            msk: Zeroizing::new(response.msk.to_vec()),
            k_aut: Zeroizing::new(response.k_aut.to_vec()),
        }),
        Ok(EapAkaExchangeResult::SynchronizationFailure(packet)) => Ok(EapAkaResponse {
            outcome: "synchronization_failure",
            packet,
            msk: Zeroizing::new(Vec::new()),
            k_aut: Zeroizing::new(Vec::new()),
        }),
        Err(EapAkaExchangeError::Eap(_)) => Err(EapAkaRequestError::Eap),
        Err(EapAkaExchangeError::SimService(error)) => {
            Err(EapAkaRequestError::Authentication(error))
        }
        Err(EapAkaExchangeError::AuthenticationRejected) => {
            Err(EapAkaRequestError::AuthenticationRejected)
        }
    }
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    exchanges: &crate::eap_identity::EapExchangeStore,
    owner: crate::reader_pcsc::Owner,
    gate: std::sync::Arc<crate::reader_reservation::Gate>,
) -> Response<Full<Bytes>> {
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        32_768,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(value) => value,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return failure(StatusCode::BAD_REQUEST, "invalid_request"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
    };
    let Ok(mut input) = serde_json::from_slice::<EapAkaRequest>(&body) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if input
        .pcsc
        .as_ref()
        .is_some_and(|binding| binding.validate_for_reader(input.reader.trim()).is_err())
    {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_eap_aka_input");
    }
    let Some(mut exchange) = exchanges.take(&input.exchange_id) else {
        return failure(StatusCode::GONE, "eap_exchange_expired_or_consumed");
    };
    if !exchange.bind(&input.reader, &input.application) {
        return failure(StatusCode::CONFLICT, "eap_exchange_reader_mismatch");
    }
    if let Some(expected) = &input.expected_identity {
        let application = match input.application.as_str() {
            "usim" => AkaApplication::Usim,
            "isim" => AkaApplication::Isim,
            _ => return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_eap_aka_input"),
        };
        if crate::reader_aka_binding::ReaderAkaBinding::new(
            &input.reader,
            application,
            &expected.iccid,
            &expected.imsi,
        )
        .is_err()
        {
            return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_eap_aka_input");
        }
    }
    if !exchange.bind_identity(input.expected_identity.as_ref()) {
        return failure(StatusCode::CONFLICT, "reader_identity_changed");
    }
    let expected_identity = input.expected_identity.clone();
    let worker_reader = input.reader.trim().to_owned();
    let transaction = input.transaction.take();
    let physical = input.pcsc.clone();
    let result = crate::reader_pcsc::run_card_work(
        owner.clone(),
        gate,
        &worker_reader,
        transaction.as_ref(),
        physical.as_ref(),
        move || {
            let pcsc = input.pcsc.clone();
            let mut owner_failure = None;
            let result = process(
                input,
                &exchange.transcript,
                |reader, application, rand, autn| {
                    // Validate the EAP transcript/challenge before entering
                    // physical ownership. Only its actual SIM callback opens
                    // or borrows a transaction.
                    owner
                        .with_transaction(pcsc.as_ref(), reader, |transport| {
                            if let Some(expected) = &expected_identity {
                                proletarius_reader::authenticate_bound_with_transport(
                                    transport,
                                    application,
                                    &expected.iccid,
                                    &expected.imsi,
                                    rand,
                                    autn,
                                )
                            } else {
                                proletarius_reader::authenticate_auto_with_transport(
                                    transport,
                                    application,
                                    rand,
                                    autn,
                                )
                            }
                        })
                        .unwrap_or_else(|error| {
                            owner_failure = Some(error);
                            Err(AuthenticationError::Exchange(
                                "native reader ownership failed".into(),
                            ))
                        })
                },
            );
            if let Some(error) = owner_failure {
                return Err(EapAkaRequestError::Owner(error));
            }
            result.map(|response| (response, exchange))
        },
        READER_EAP_AKA_OPERATION_TIMEOUT,
    )
    .await;
    match result {
        Ok(Ok((response, exchange))) => {
            let next_exchange_id = if response.outcome == "synchronization_failure" {
                match exchanges.resume_after_sync_failure(exchange) {
                    Ok(id) => Some(id),
                    Err(code) => return failure(StatusCode::CONFLICT, code),
                }
            } else {
                None
            };
            let payload = json!({
                "next_exchange_id": next_exchange_id,
                "outcome": response.outcome,
                "eap_response": response.packet.as_slice(),
                "msk": response.msk.as_slice(),
                "k_aut": response.k_aut.as_slice(),
            });
            crate::json_response(StatusCode::OK, &payload)
        }
        Ok(Err(EapAkaRequestError::InvalidInput)) => {
            failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_eap_aka_input")
        }
        Ok(Err(EapAkaRequestError::Owner(error))) => crate::reader_auth::owner_failure(error),
        Ok(Err(EapAkaRequestError::Eap)) => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_eap_aka_challenge",
        ),
        Ok(Err(EapAkaRequestError::AuthenticationRejected)) => {
            failure(StatusCode::FORBIDDEN, "reader_authentication_rejected")
        }
        Ok(Err(EapAkaRequestError::Authentication(error))) => {
            crate::diagnostics::write_line(format_args!(
                "switchd-rs reader EAP-AKA failed: {error}"
            ));
            let code = error.failure_code();
            let status = match code {
                "reader_card_absent" | "reader_unavailable" | "reader_identity_changed" => {
                    StatusCode::CONFLICT
                }
                "reader_busy" => StatusCode::SERVICE_UNAVAILABLE,
                "reader_application_unavailable" => StatusCode::NOT_FOUND,
                _ => StatusCode::BAD_GATEWAY,
            };
            failure(status, code)
        }
        Err(crate::reader_task::ReaderTaskError::Busy) => {
            failure(StatusCode::CONFLICT, "card_busy")
        }
        Err(crate::reader_task::ReaderTaskError::WorkerFailed) => {
            failure(StatusCode::INTERNAL_SERVER_ERROR, "reader_task_failed")
        }
        Err(crate::reader_task::ReaderTaskError::Timeout) => {
            failure(StatusCode::GATEWAY_TIMEOUT, "reader_authentication_timeout")
        }
    }
}

fn failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({
            "error":"Rust EAP-AKA 读卡器鉴权失败",
            "code":code,
            "retryable":false
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use zeroize::Zeroizing;

    use super::{EapAkaRequest, EapAkaRequestError, process};
    use proletarius_reader::AkaApplication;
    use proletarius_sim::AkaResult;

    fn append_attribute(kind: u8, value: &[u8], output: &mut Vec<u8>) {
        let padded_length = (value.len() + 2).div_ceil(4) * 4;
        output.push(kind);
        output.push((padded_length / 4) as u8);
        output.extend_from_slice(value);
        output.resize(output.len() + padded_length - value.len() - 2, 0);
    }

    fn valid_challenge(identifier: u8) -> Vec<u8> {
        let mut attributes = Vec::new();
        let mut rand = vec![0, 0];
        rand.extend_from_slice(&[0x11; 16]);
        let mut autn = vec![0, 0];
        autn.extend_from_slice(&[0x22; 16]);
        append_attribute(1, &rand, &mut attributes);
        append_attribute(2, &autn, &mut attributes);
        append_attribute(11, &[0; 18], &mut attributes);
        let length = 8 + attributes.len();
        let mut packet = vec![1, identifier, length as u8, 23, 1, 0, 0];
        packet.insert(2, (length >> 8) as u8);
        packet.extend_from_slice(&attributes);
        packet
    }

    fn valid_request() -> (EapAkaRequest, Zeroizing<Vec<u8>>) {
        let identity = b"nai@example".to_vec();
        let mut response = vec![2, 9, 0, (5 + identity.len()) as u8, 1];
        response.extend_from_slice(&identity);
        (
            EapAkaRequest {
                pcsc: None,
                transaction: None,
                reader: " Fixture Reader ".into(),
                application: "usim".into(),
                exchange_id: "ab".repeat(32),
                expected_identity: None,
                challenge: valid_challenge(77),
                checkcode_mode: "minimal".into(),
            },
            Zeroizing::new([&[1, 9, 0, 5, 1][..], response.as_slice()].concat()),
        )
    }

    #[test]
    fn malformed_challenge_fails_before_reader_authentication() {
        let (mut input, transcript) = valid_request();
        input.challenge = vec![1, 77, 0, 8, 23, 1, 0, 0];
        let called = Cell::new(false);
        let result = process(input, &transcript, |_, _, _, _| {
            called.set(true);
            Ok(AkaResult::AuthenticationReject)
        });
        assert!(matches!(result, Err(EapAkaRequestError::Eap)));
        assert!(!called.get());
    }

    #[test]
    fn malformed_identity_transcript_fails_before_reader_authentication() {
        let (input, mut transcript) = valid_request();
        transcript[1] = 8;
        let called = Cell::new(false);
        let result = process(input, &transcript, |_, _, _, _| {
            called.set(true);
            Ok(AkaResult::AuthenticationReject)
        });
        assert!(matches!(result, Err(EapAkaRequestError::InvalidInput)));
        assert!(!called.get());
    }

    #[test]
    fn synchronization_failure_uses_selected_usim_and_preserves_request_id() {
        let (input, transcript) = valid_request();
        let calls = Cell::new(0);
        let response = process(input, &transcript, |reader, application, rand, autn| {
            calls.set(calls.get() + 1);
            assert_eq!(reader, "Fixture Reader");
            assert_eq!(application, AkaApplication::Usim);
            assert_eq!(rand, &[0x11; 16]);
            assert_eq!(autn, &[0x22; 16]);
            Ok(AkaResult::SynchronizationFailure { auts: [0x5a; 14] })
        });
        let response = response.unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(response.outcome, "synchronization_failure");
        assert_eq!(&response.packet[0..2], &[2, 77]);
        assert_eq!(&response.packet[4..8], &[23, 4, 0, 0]);
        assert!(response.msk.is_empty());
        assert!(response.k_aut.is_empty());
    }
}
