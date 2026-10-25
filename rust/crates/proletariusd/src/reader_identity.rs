//! Authenticated, read-only reader identity endpoint.

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct IdentityRequest {
    reader: String,
    #[serde(default)]
    pcsc: Option<crate::reader_pcsc::Binding>,
    #[serde(default)]
    aid: Vec<u8>,
    #[serde(default)]
    transaction: Option<crate::reader_reservation::CardTransaction>,
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    owner: crate::reader_pcsc::Owner,
    gate: std::sync::Arc<crate::reader_reservation::Gate>,
) -> Response<Full<Bytes>> {
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        4096,
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
    let Ok(input) = serde_json::from_slice::<IdentityRequest>(&body) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if input.reader.trim().is_empty()
        || input
            .pcsc
            .as_ref()
            .is_some_and(|binding| binding.validate_for_reader(input.reader.trim()).is_err())
        || (!input.aid.is_empty() && !(5..=16).contains(&input.aid.len()))
        || !has_consistent_workflow_binding(&input)
    {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_identity_input");
    }
    let worker_reader = input.reader.trim().to_owned();
    let aid = input.aid;
    let transaction = input.transaction;
    let pcsc = input.pcsc;
    let bound_workflow = transaction.is_some();
    let task = if let (Some(transaction), Some(pcsc)) = (transaction, pcsc) {
        let reader_for_io = worker_reader.clone();
        let owner_for_io = owner.clone();
        let pcsc_for_io = pcsc.clone();
        crate::reader_pcsc::run_card_work(
            owner,
            gate,
            &worker_reader,
            Some(&transaction),
            Some(&pcsc),
            move || {
                owner_for_io.with_transaction(Some(&pcsc_for_io), &reader_for_io, |transport| {
                    read_identity(transport, &aid)
                })
            },
            std::time::Duration::from_secs(45),
        )
        .await
    } else {
        let reader_for_io = worker_reader.clone();
        crate::reader_aka_binding::run_owned(
            &worker_reader,
            move || {
                owner.with_transaction(None, &reader_for_io, |transport| {
                    read_identity(transport, &aid)
                })
            },
            std::time::Duration::from_secs(45),
        )
        .await
    };
    match task {
        Ok(Ok(Ok(metadata))) => crate::json_response(
            StatusCode::OK,
            &json!({
                "ok": true,
                "iccid": metadata.iccid,
                "imsi": metadata.imsi,
                "mcc": metadata.mcc,
                "mnc": metadata.mnc,
                "application_aid": metadata.application_aid,
                "gid1": metadata.gid1,
                "gid2": metadata.gid2,
                "spn": metadata.spn,
                "phone_number": metadata.phone_number,
                "operator_name": metadata.operator_name,
                "operator_source": metadata.operator_source,
                "smsc": metadata.smsc,
                "smsc_psi": metadata.smsc_psi,
                "smsc_target": metadata.smsc_target,
                "private_identity": metadata.private_identity,
                "public_identity": metadata.public_identity,
                "home_domain": metadata.home_domain,
                "isim_aid": metadata.isim_aid,
                "source": "rust_pcsc",
            }),
        ),
        Ok(Err(error)) => crate::reader_auth::owner_failure(error),
        Ok(Ok(Err(error))) => {
            crate::diagnostics::write_line(format_args!(
                "switchd-rs reader identity failed: {error}"
            ));
            let code = error.failure_code();
            let status = match code {
                "reader_busy" => StatusCode::SERVICE_UNAVAILABLE,
                "reader_card_absent" | "reader_unavailable" => StatusCode::CONFLICT,
                _ => StatusCode::BAD_GATEWAY,
            };
            failure(status, code)
        }
        Err(error) => {
            let code = match (bound_workflow, error) {
                (true, crate::reader_task::ReaderTaskError::Busy) => "reader_busy",
                (false, crate::reader_task::ReaderTaskError::Busy) => "card_busy",
                (_, crate::reader_task::ReaderTaskError::Timeout) => "reader_identity_timeout",
                (_, crate::reader_task::ReaderTaskError::WorkerFailed) => "reader_task_failed",
            };
            let status = if code == "reader_busy" || code == "card_busy" {
                StatusCode::CONFLICT
            } else if code == "reader_identity_timeout" {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            failure(status, code)
        }
    }
}

fn read_identity<T: proletarius_sim::ApduTransport>(
    transport: &mut T,
    aid: &[u8],
) -> Result<proletarius_reader::ReaderSimMetadata, proletarius_reader::IdentityError>
where
    T::Error: std::fmt::Display,
{
    proletarius_reader::read_sim_metadata_with_transport(transport, aid)
}

fn has_consistent_workflow_binding(input: &IdentityRequest) -> bool {
    input.transaction.is_some() == input.pcsc.is_some()
}

fn failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"读取读卡器身份失败","code":code,"retryable":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_reader_or_short_aid() {
        for input in [
            IdentityRequest {
                reader: " ".into(),
                pcsc: None,
                aid: vec![0; 7],
                transaction: None,
            },
            IdentityRequest {
                reader: "Reader".into(),
                pcsc: None,
                aid: vec![0; 4],
                transaction: None,
            },
        ] {
            assert!(
                input.reader.trim().is_empty()
                    || (!input.aid.is_empty() && !(5..=16).contains(&input.aid.len()))
            );
        }
    }

    #[test]
    fn missing_aid_is_valid_for_native_ef_dir_discovery() {
        let request: IdentityRequest = serde_json::from_str(r#"{"reader":"Reader"}"#).unwrap();
        assert!(request.aid.is_empty());
        assert!(request.transaction.is_none());
        assert!(!request.reader.trim().is_empty());
    }

    #[test]
    fn identity_request_rejects_partial_workflow_binding() {
        let request: IdentityRequest = serde_json::from_str(r#"{"reader":"Reader"}"#).unwrap();
        assert!(has_consistent_workflow_binding(&request));

        let partial: IdentityRequest = serde_json::from_str(
            r#"{"reader":"Reader","pcsc":{"producer_id":"producer","reader":"Reader","session_id":"session","lease_id":"lease","lease_seq":1}}"#,
        )
        .unwrap();
        assert!(!has_consistent_workflow_binding(&partial));
    }
}
