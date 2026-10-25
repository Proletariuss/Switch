//! Frozen pre-505 operation HTTP oracle; typed admission remains in the parent.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestBinding {
    binding: OperationBinding,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Quiesce {
    producer_id: String,
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    owner: Owner,
    gate: Arc<Gate>,
) -> Response<Full<Bytes>> {
    let action = request
        .uri()
        .path()
        .strip_prefix(PREFIX)
        .and_then(|p| p.strip_prefix("operation/"))
        .unwrap_or("")
        .to_owned();
    if request.method() != hyper::Method::POST
        || !matches!(
            action.as_str(),
            "acquire" | "status" | "release" | "renew" | "quiesce"
        )
    {
        return reject("route_not_found", StatusCode::NOT_FOUND);
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(2),
        Limited::new(request.into_body(), 8192).collect(),
    )
    .await
    {
        Ok(Ok(v)) => v.to_bytes(),
        _ => return reject("invalid_request", StatusCode::BAD_REQUEST),
    };
    let result = if action == "quiesce" {
        serde_json::from_slice::<Quiesce>(&bytes)
            .map_err(|_| "invalid_request")
            .and_then(|q| gate.quiesce_operations(&q.producer_id))
    } else {
        (|| {
            let b = serde_json::from_slice::<RequestBinding>(&bytes)
                .map_err(|_| "invalid_request")?
                .binding;
            if action == "acquire" {
                let (value, start) = gate.operation_acquire(&b)?;
                if start {
                    tokio::spawn(drive(gate.clone(), owner, b));
                }
                Ok(value)
            } else {
                gate.operation_command(&b, &action)
            }
        })()
    };
    match result {
        Ok(v) => crate::json_response(StatusCode::OK, &v),
        Err(code) => reject(
            code,
            if matches!(code, "invalid_request" | "invalid_reader_binding") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::CONFLICT
            },
        ),
    }
}
