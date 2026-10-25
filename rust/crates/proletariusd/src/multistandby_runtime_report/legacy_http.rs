//! Test-only adapter for the retired internal observation HTTP envelope.
use super::*;
use http_body_util::{BodyExt, Limited};
use hyper::body::Body;

pub(crate) const INTERNAL_OBSERVE_PATH: &str = "/api/internal/multistandby/runtime/observe";
pub(crate) const INTERNAL_REGISTRATION_SCHEDULE_PATH: &str =
    "/api/internal/multistandby/runtime/registration-schedule";

pub(crate) async fn handle_internal<B>(
    request: Request<B>,
    journal: Arc<Mutex<Journal>>,
) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if request.method() != Method::POST || request.uri().path() != INTERNAL_OBSERVE_PATH {
        return failure(StatusCode::NOT_FOUND, "route_not_found", "route not found");
    }
    let body = match Limited::new(request.into_body(), BODY_LIMIT)
        .collect()
        .await
    {
        Ok(value) => value.to_bytes(),
        Err(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_runtime_observation",
                "invalid runtime observation",
            );
        }
    };
    let Ok(input) = serde_json::from_slice::<ObservationInput>(&body) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_runtime_observation",
            "invalid runtime observation",
        );
    };
    let at_ms = now_ms();
    let Some(event) = observation_event(input, at_ms) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_runtime_observation",
            "invalid runtime observation",
        );
    };
    let state = event.state.clone();
    let recovery = event.recovery;
    let mut journal = journal
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    journal.record_at(event, at_ms);
    crate::json_response(
        StatusCode::OK,
        &json!({"stored":true,"state":state,"recovery":recovery,"storage_issue":journal.storage_issue}),
    )
}

pub(crate) async fn handle_registration_schedule<B>(
    request: Request<B>,
    journal: Arc<Mutex<Journal>>,
) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Send + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if request.method() != Method::POST
        || request.uri().path() != INTERNAL_REGISTRATION_SCHEDULE_PATH
    {
        return failure(StatusCode::NOT_FOUND, "route_not_found", "route not found");
    }
    let body = match Limited::new(request.into_body(), BODY_LIMIT)
        .collect()
        .await
    {
        Ok(value) => value.to_bytes(),
        Err(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_registration_schedule",
                "invalid registration schedule request",
            );
        }
    };
    let Ok(input) = serde_json::from_slice::<RegistrationScheduleRequest>(&body) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_registration_schedule",
            "invalid registration schedule request",
        );
    };
    let mut journal = journal
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match journal.schedule_registration(input, now_ms()) {
        Ok(Some(schedule)) => crate::json_response(
            StatusCode::OK,
            &json!({
                "scheduled": schedule.due_at_ms.is_some(),
                "generation": schedule.generation,
                "due_at_ms": schedule.due_at_ms.unwrap_or_default()
            }),
        ),
        Ok(None) => failure(
            StatusCode::BAD_REQUEST,
            "invalid_registration_schedule",
            "invalid registration schedule request",
        ),
        Err(()) if !journal.storage_issue.is_empty() => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime_history_unavailable",
            "N待注册续期调度暂不可持久化",
        ),
        Err(()) => failure(
            StatusCode::CONFLICT,
            "registration_schedule_conflict",
            "stale or conflicting registration schedule",
        ),
    }
}
