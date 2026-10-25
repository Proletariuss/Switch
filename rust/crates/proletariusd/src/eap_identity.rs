//! Bounded EAP Identity handling and short-lived IKE_AUTH exchange storage.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use proletarius_ike::eap_aka::AkaCheckcodeMode;
use proletarius_vowifi::Ikev2EapConversation;
use serde::Deserialize;
use serde_json::json;
use sha1::{Digest, Sha1};
use zeroize::Zeroizing;

const MAX_EAP_IDENTITY_REQUEST: usize = 4_096;
const MAX_EAP_IDENTITY: usize = 2_048;
const MAX_IDENTITY_TRANSCRIPT: usize = 8_192;
const MAX_ACTIVE_EXCHANGES: usize = 64;
const EXCHANGE_TTL: Duration = Duration::from_secs(120);
const EXCHANGE_ID_BYTES: usize = 32;

pub(crate) struct EapExchange {
    pub(crate) transcript: Zeroizing<Vec<u8>>,
    expires_at: Instant,
    remaining_sync_rounds: u8,
    binding: Option<(String, String)>,
    identity_binding: Option<Option<crate::reader_auth::ExpectedIdentity>>,
}

/// Keeps sensitive EAP transcript bytes inside the Rust network-service owner.
/// IDs are random bearer capabilities, short-lived, bounded and single-use for
/// AKA. The mutex is held only while manipulating this in-memory map.
#[derive(Default)]
pub(crate) struct EapExchangeStore {
    exchanges: Mutex<HashMap<String, EapExchange>>,
}

impl EapExchangeStore {
    fn insert(&self, transcript: Vec<u8>) -> Result<String, &'static str> {
        let transcript = Zeroizing::new(transcript);
        if transcript.is_empty() || transcript.len() > MAX_IDENTITY_TRANSCRIPT {
            return Err("invalid_eap_identity_input");
        }
        self.insert_exchange(EapExchange {
            transcript,
            expires_at: Instant::now() + EXCHANGE_TTL,
            remaining_sync_rounds: 2,
            binding: None,
            identity_binding: None,
        })
    }

    fn insert_exchange(&self, exchange: EapExchange) -> Result<String, &'static str> {
        if exchange.expires_at <= Instant::now() {
            return Err("eap_exchange_expired_or_consumed");
        }
        let mut exchanges = self
            .exchanges
            .lock()
            .map_err(|_| "eap_exchange_store_unavailable")?;
        prune_expired(&mut exchanges);
        if exchanges.len() >= MAX_ACTIVE_EXCHANGES {
            return Err("eap_exchange_capacity_reached");
        }

        for _ in 0..3 {
            let mut random = [0_u8; EXCHANGE_ID_BYTES];
            getrandom::fill(&mut random).map_err(|_| "eap_exchange_id_unavailable")?;
            let id = encode_exchange_id(&random);
            if let std::collections::hash_map::Entry::Vacant(entry) = exchanges.entry(id.clone()) {
                entry.insert(exchange);
                return Ok(id);
            }
        }
        Err("eap_exchange_id_collision")
    }

    /// Atomically removes and returns an exchange. Replays cannot reuse a
    /// transcript or trigger a second physical AKA operation.
    pub(crate) fn take(&self, id: &str) -> Option<EapExchange> {
        if !valid_exchange_id(id) {
            return None;
        }
        let mut exchanges = self.exchanges.lock().ok()?;
        prune_expired(&mut exchanges);
        exchanges.remove(id)
    }

    pub(crate) fn resume_after_sync_failure(
        &self,
        mut exchange: EapExchange,
    ) -> Result<String, &'static str> {
        if exchange.remaining_sync_rounds == 0 {
            return Err("eap_sync_retry_limit");
        }
        exchange.remaining_sync_rounds -= 1;
        self.insert_exchange(exchange)
    }

    /// Releases an exchange when IKE_AUTH ends before the AKA request arrives.
    pub(crate) fn release(&self, id: &str) -> bool {
        if !valid_exchange_id(id) {
            return false;
        }
        let Ok(mut exchanges) = self.exchanges.lock() else {
            return false;
        };
        prune_expired(&mut exchanges);
        exchanges.remove(id).is_some()
    }

    pub(crate) fn prune(&self) {
        if let Ok(mut exchanges) = self.exchanges.lock() {
            prune_expired(&mut exchanges);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.exchanges.lock().map_or(0, |exchanges| exchanges.len())
    }
}

impl EapExchange {
    pub(crate) fn bind_identity(
        &mut self,
        identity: Option<&crate::reader_auth::ExpectedIdentity>,
    ) -> bool {
        match &self.identity_binding {
            Some(bound) => bound.as_ref() == identity,
            None => {
                self.identity_binding = Some(identity.cloned());
                true
            }
        }
    }

    pub(crate) fn bind(&mut self, reader: &str, application: &str) -> bool {
        let requested = (reader.trim().to_owned(), application.to_owned());
        match &self.binding {
            Some(binding) => *binding == requested,
            None => {
                self.binding = Some(requested);
                true
            }
        }
    }
}

fn prune_expired(exchanges: &mut HashMap<String, EapExchange>) {
    let now = Instant::now();
    exchanges.retain(|_, exchange| exchange.expires_at > now);
}

fn encode_exchange_id(bytes: &[u8; EXCHANGE_ID_BYTES]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(EXCHANGE_ID_BYTES * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn valid_exchange_id(id: &str) -> bool {
    id.len() == EXCHANGE_ID_BYTES * 2
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Deserialize)]
struct EapIdentityRequest {
    identity: Vec<u8>,
    request: Vec<u8>,
}

#[derive(Deserialize)]
struct ReleaseExchangeRequest {
    exchange_id: String,
}

fn process(input: EapIdentityRequest) -> Result<(Vec<u8>, Vec<u8>), &'static str> {
    if input.identity.is_empty()
        || input.identity.len() > MAX_EAP_IDENTITY
        || input.request.len() < 5
        || input.request.len() > MAX_EAP_IDENTITY_REQUEST
        || input.request[0] != 1
        || usize::from(u16::from_be_bytes([input.request[2], input.request[3]]))
            != input.request.len()
    {
        return Err("invalid_eap_identity_input");
    }
    let mut conversation = Ikev2EapConversation::new(&input.identity, AkaCheckcodeMode::Minimal)
        .map_err(|_| "invalid_eap_identity_input")?;
    let response = conversation
        .respond_identity(&input.request)
        .map_err(|_| "invalid_eap_identity_request")?;
    let mut transcript = Vec::with_capacity(input.request.len() + response.len());
    transcript.extend_from_slice(&input.request);
    transcript.extend_from_slice(&response);
    Ok((response, transcript))
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    exchanges: &EapExchangeStore,
) -> Response<Full<Bytes>> {
    if request.method() == Method::POST && request.uri().path() == "/api/v2/vowifi/eap-identity" {
        let body = match crate::http_server::collect_body_with_timeout(
            std::pin::pin!(request.into_body()),
            32_768,
            Duration::from_secs(2),
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
        let Ok(input) = serde_json::from_slice::<EapIdentityRequest>(&body) else {
            return failure(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return match process(input) {
            Ok((packet, transcript)) => {
                let checkcode = Sha1::digest(&transcript);
                match exchanges.insert(transcript) {
                    Ok(exchange_id) => crate::json_response(
                        StatusCode::OK,
                        &json!({
                            "eap_response": packet,
                            "exchange_id": exchange_id,
                            "checkcode": checkcode.as_slice(),
                        }),
                    ),
                    Err(code) => failure(StatusCode::SERVICE_UNAVAILABLE, code),
                }
            }
            Err(code) => failure(StatusCode::UNPROCESSABLE_ENTITY, code),
        };
    }

    if request.method() == Method::POST
        && request.uri().path() == "/api/v2/vowifi/eap-exchange/release"
    {
        let body = match crate::http_server::collect_body_with_timeout(
            std::pin::pin!(request.into_body()),
            4_096,
            Duration::from_secs(2),
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
        let Ok(input) = serde_json::from_slice::<ReleaseExchangeRequest>(&body) else {
            return failure(StatusCode::BAD_REQUEST, "invalid_request");
        };
        if !valid_exchange_id(&input.exchange_id) {
            return failure(StatusCode::BAD_REQUEST, "invalid_exchange_id");
        }
        exchanges.release(&input.exchange_id);
        return crate::json_response(StatusCode::OK, &json!({}));
    }

    failure(StatusCode::NOT_FOUND, "route_not_found")
}

fn failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({ "error": "Rust EAP Identity response failed", "code": code }),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Instant;

    use super::{
        EXCHANGE_TTL, EapExchange, EapExchangeStore, EapIdentityRequest, encode_exchange_id,
        process, prune_expired, valid_exchange_id,
    };
    use zeroize::Zeroizing;

    #[test]
    fn identity_response_preserves_identifier_and_exact_nai() {
        let identity = b"0001010123456789@nai.epc.mnc001.mcc001.3gppnetwork.org";
        let request = EapIdentityRequest {
            identity: identity.to_vec(),
            request: vec![1, 27, 0, 5, 1],
        };
        let (response, transcript) = process(request).unwrap();
        assert_eq!(response[0], 2);
        assert_eq!(response[1], 27);
        assert_eq!(
            usize::from(u16::from_be_bytes([response[2], response[3]])),
            response.len()
        );
        assert_eq!(response[4], 1);
        assert_eq!(&response[5..], identity);
        assert_eq!(&transcript[..5], &[1, 27, 0, 5, 1]);
        assert_eq!(&transcript[5..], response);
    }

    #[test]
    fn malformed_identity_request_is_rejected_without_partial_response() {
        for request in [
            vec![1, 3, 0, 5, 23],
            vec![1, 3, 0, 6, 1],
            vec![2, 3, 0, 5, 1],
        ] {
            assert!(
                process(EapIdentityRequest {
                    identity: b"nai@example".to_vec(),
                    request,
                })
                .is_err()
            );
        }
    }

    #[test]
    fn empty_or_oversized_identity_is_rejected() {
        assert_eq!(
            process(EapIdentityRequest {
                identity: Vec::new(),
                request: vec![1, 7, 0, 5, 1],
            })
            .unwrap_err(),
            "invalid_eap_identity_input"
        );
        assert_eq!(
            process(EapIdentityRequest {
                identity: vec![b'x'; 2049],
                request: vec![1, 7, 0, 5, 1],
            })
            .unwrap_err(),
            "invalid_eap_identity_input"
        );
    }

    #[test]
    fn exchange_ids_are_unpredictable_shape_tokens_and_single_use() {
        let store = EapExchangeStore::default();
        let id = store.insert(vec![1, 2, 3, 4]).unwrap();
        assert!(valid_exchange_id(&id));
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.take(&id).unwrap().transcript.as_slice(),
            &[1, 2, 3, 4]
        );
        assert!(store.take(&id).is_none());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn expired_exchanges_are_pruned_before_access() {
        let store = EapExchangeStore::default();
        let id = "ab".repeat(32);
        store.exchanges.lock().unwrap().insert(
            id.clone(),
            EapExchange {
                transcript: Zeroizing::new(vec![1, 2, 3]),
                expires_at: Instant::now() - EXCHANGE_TTL,
                remaining_sync_rounds: 2,
                binding: None,
                identity_binding: None,
            },
        );
        assert!(store.take(&id).is_none());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn pruning_releases_expired_transcripts_and_id_validation_is_strict() {
        let expired_id = encode_exchange_id(&[0x11; 32]);
        let mut exchanges = HashMap::from([(
            expired_id,
            EapExchange {
                transcript: Zeroizing::new(vec![0x44; 12]),
                expires_at: Instant::now() - EXCHANGE_TTL,
                remaining_sync_rounds: 2,
                binding: None,
                identity_binding: None,
            },
        )]);
        prune_expired(&mut exchanges);
        assert!(exchanges.is_empty());
        assert!(!valid_exchange_id("../".repeat(21).as_str()));
        assert!(!valid_exchange_id(&"A".repeat(64)));
    }

    #[test]
    fn capacity_and_release_are_bounded_and_idempotent() {
        let store = EapExchangeStore::default();
        let ids: Vec<_> = (0..super::MAX_ACTIVE_EXCHANGES)
            .map(|_| store.insert(vec![1]).unwrap())
            .collect();
        assert_eq!(
            store.insert(vec![2]).unwrap_err(),
            "eap_exchange_capacity_reached"
        );
        assert!(store.release(&ids[0]));
        assert!(!store.release(&ids[0]));
        assert!(store.insert(vec![3]).is_ok());
        assert_eq!(store.len(), super::MAX_ACTIVE_EXCHANGES);
    }

    #[test]
    fn sync_continuation_is_bound_bounded_and_keeps_original_deadline() {
        let store = EapExchangeStore::default();
        let first = store.insert(vec![1, 2, 3]).unwrap();
        let mut exchange = store.take(&first).unwrap();
        let deadline = exchange.expires_at;
        assert!(exchange.bind("Reader A", "usim"));
        let identity = crate::reader_auth::ExpectedIdentity {
            iccid: "8901234567890123456".into(),
            imsi: "234100896392986".into(),
        };
        assert!(exchange.bind_identity(Some(&identity)));
        let second = store.resume_after_sync_failure(exchange).unwrap();
        assert_ne!(first, second);
        assert!(store.take(&first).is_none());
        let mut exchange = store.take(&second).unwrap();
        assert_eq!(exchange.expires_at, deadline);
        assert!(!exchange.bind("Reader B", "usim"));
        assert!(!exchange.bind_identity(None));
        let mut other = identity.clone();
        other.imsi = "234100896392987".into();
        assert!(!exchange.bind_identity(Some(&other)));
        assert!(exchange.bind_identity(Some(&identity)));
        assert!(!exchange.bind("Reader A", "isim"));
        assert!(exchange.bind("Reader A", "usim"));
        let third = store.resume_after_sync_failure(exchange).unwrap();
        let exchange = store.take(&third).unwrap();
        assert_eq!(
            store.resume_after_sync_failure(exchange).unwrap_err(),
            "eap_sync_retry_limit"
        );
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn concurrent_consumers_cannot_replay_exchange() {
        let store = std::sync::Arc::new(EapExchangeStore::default());
        let id = store.insert(vec![1, 2, 3]).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let id = id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.take(&id).is_some()
                })
            })
            .collect();
        let consumed = tasks
            .into_iter()
            .filter_map(|task| task.join().ok())
            .filter(|value| *value)
            .count();
        assert_eq!(consumed, 1);
    }
}
