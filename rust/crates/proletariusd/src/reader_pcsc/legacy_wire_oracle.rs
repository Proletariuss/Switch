//! Frozen pre-505 HTTP oracle. Never compiled into the production gateway.
//! Exercises physical cancellation and authority using the retired wire shape.
use super::*;
use http_body_util::{BodyExt, Limited};
use hyper::{Method, Request, body::Incoming};
const MAX_BODY: usize = 160 * 1024;
const DEFAULT_TTL: u64 = 120_000;

fn ttl(value: Option<u64>) -> Result<Duration, Failure> {
    let value = value.unwrap_or(DEFAULT_TTL);
    if !(1_000..=MAX_TTL).contains(&value) {
        Err(Failure::invalid())
    } else {
        Ok(Duration::from_millis(value))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    producer_id: String,
    reader: String,
    session_id: String,
    session_seq: u64,
    #[serde(rename = "transaction")]
    _transaction: Option<crate::reader_reservation::CardTransaction>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseRequest {
    producer_id: String,
    reader: String,
    session_id: String,
    lease_id: String,
    lease_seq: u64,
    ttl_ms: Option<u64>,
    #[serde(rename = "transaction")]
    _transaction: Option<crate::reader_reservation::CardTransaction>,
}
impl LeaseRequest {
    fn binding(&self) -> Binding {
        Binding {
            producer_id: self.producer_id.clone(),
            reader: self.reader.clone(),
            session_id: self.session_id.clone(),
            lease_id: self.lease_id.clone(),
            lease_seq: self.lease_seq,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransmitRequest {
    producer_id: String,
    reader: String,
    session_id: String,
    lease_id: String,
    lease_seq: u64,
    operation_id: String,
    operation_seq: u64,
    command_hex: String,
    #[serde(rename = "transaction")]
    _transaction: Option<crate::reader_reservation::CardTransaction>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetRequest {
    producer_id: String,
    reader: String,
    session_id: String,
    lease_id: String,
    lease_seq: u64,
    operation_id: String,
    operation_seq: u64,
    action: ResetAction,
    #[serde(rename = "transaction")]
    _transaction: Option<crate::reader_reservation::CardTransaction>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseRequest {
    producer_id: String,
    reader: String,
    session_id: String,
    session_seq: u64,
    lease_id: Option<String>,
    lease_seq: Option<u64>,
}
pub(crate) async fn handle(
    request: Request<Incoming>,
    owner: Owner,
    stopping: bool,
    gate: Arc<crate::reader_reservation::Gate>,
) -> Response<Full<Bytes>> {
    let action = request
        .uri()
        .path()
        .strip_prefix(PREFIX)
        .unwrap_or("")
        .to_owned();
    if request.method() != Method::POST {
        return crate::json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            &json!({"error":"method not allowed"}),
        );
    }
    if ![
        "open", "status", "acquire", "renew", "release", "transmit", "reset", "close",
    ]
    .contains(&action.as_str())
    {
        return crate::json_response(StatusCode::NOT_FOUND, &json!({"error":"not found"}));
    }
    if stopping && !matches!(action.as_str(), "status" | "release" | "close") {
        return Failure::stopped().response();
    }
    let body = match Limited::new(request.into_body(), MAX_BODY).collect().await {
        Ok(body) => body.to_bytes(),
        Err(_) => return Failure::invalid().response(),
    };
    // Compatibility consumers may borrow a fixed native session, but never
    // create or close the lifecycle owner's session through the wire adapter.
    if gate.native_lifecycle_owned() && matches!(action.as_str(), "open" | "close") {
        return Failure::new(
            StatusCode::CONFLICT,
            "reader_native_lifecycle_owned",
            "Reader session lifecycle is owned by the native service",
        )
        .response();
    }
    // Register physical ownership before queueing. Cancellation of the HTTP
    // waiter cannot release the permit once the blocking worker has started.
    let physical = if matches!(action.as_str(), "open" | "acquire" | "transmit" | "reset") {
        #[derive(Deserialize)]
        struct Authority {
            producer_id: String,
            reader: String,
            session_id: String,
            session_seq: Option<u64>,
            lease_id: Option<String>,
            lease_seq: Option<u64>,
            transaction: Option<crate::reader_reservation::CardTransaction>,
        }
        let authority: Authority = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return Failure::invalid().response(),
        };
        let Some(transaction) = authority.transaction else {
            return Failure::invalid().response();
        };
        let permit =
            match gate.track_card_io(&transaction, &authority.producer_id, &authority.reader) {
                Ok(value) => value,
                Err(_) => return Failure::busy().response(),
            };
        if action != "open" {
            let (Some(lease_id), Some(lease_seq)) = (authority.lease_id, authority.lease_seq)
            else {
                return Failure::invalid().response();
            };
            let binding = Binding {
                producer_id: authority.producer_id,
                reader: authority.reader.clone(),
                session_id: authority.session_id,
                lease_id,
                lease_seq,
            };
            if binding.validate_for_reader(&authority.reader).is_err()
                || gate.register_card_pcsc(&transaction, &binding).is_err()
            {
                return Failure::stale().response();
            }
        } else {
            let Some(sequence) = authority.session_seq else {
                return Failure::invalid().response();
            };
            let binding = LifecycleBinding {
                producer: authority.producer_id,
                reader: authority.reader,
                session_id: authority.session_id,
                session_sequence: sequence,
            };
            if gate
                .register_card_pcsc_session(&transaction, &binding)
                .is_err()
            {
                return Failure::stale().response();
            }
        }
        Some(permit)
    } else {
        None
    };
    let pool = if matches!(action.as_str(), "status" | "renew" | "release" | "close") {
        owner.inner.control_pool.clone()
    } else {
        owner.inner.normal_pool.clone()
    };
    let job=pool.run(move||->Result<Value,Failure>{
        let _physical = physical;
        match action.as_str(){
            "open"=>{let r:OpenRequest=serde_json::from_slice(&body).map_err(|_|Failure::invalid())?;owner.open(&r.producer_id,&r.reader,&r.session_id,r.session_seq)},
            "acquire"|"renew"=>{let r:LeaseRequest=serde_json::from_slice(&body).map_err(|_|Failure::invalid())?;let duration=ttl(r.ttl_ms)?;if action=="acquire"{owner.acquire(&r.binding(),duration)}else{owner.renew(&r.binding(),duration)}},
            "release"|"status"=>{let r:LeaseRequest=serde_json::from_slice(&body).map_err(|_|Failure::invalid())?;let binding=r.binding();if action=="release"{owner.release(&binding)}else{owner.status(&binding)}},
            "transmit"=>{let r:TransmitRequest=serde_json::from_slice(&body).map_err(|_|Failure::invalid())?;let binding=Binding{producer_id:r.producer_id,reader:r.reader,session_id:r.session_id,lease_id:r.lease_id,lease_seq:r.lease_seq};binding.validate_for_reader(&binding.reader)?;let bytes=owner.transmit(&binding,None,decode_hex(&r.command_hex)?,Some((r.operation_seq,r.operation_id.clone())))?;Ok(json!({"response_hex":encode_hex(&bytes),"session_id":binding.session_id,"operation_id":r.operation_id,"operation_seq":r.operation_seq}))},
            "reset"=>{let r:ResetRequest=serde_json::from_slice(&body).map_err(|_|Failure::invalid())?;let binding=Binding{producer_id:r.producer_id,reader:r.reader,session_id:r.session_id,lease_id:r.lease_id,lease_seq:r.lease_seq};binding.validate_for_reader(&binding.reader)?;owner.reset(&binding,r.action,r.operation_seq,r.operation_id)},
            "close"=>{let r:CloseRequest=serde_json::from_slice(&body).map_err(|_|Failure::invalid())?;let binding=match(r.lease_id,r.lease_seq){(Some(id),Some(seq))=>Some(Binding{producer_id:r.producer_id.clone(),reader:r.reader.clone(),session_id:r.session_id.clone(),lease_id:id,lease_seq:seq}),(None,None)=>None,_=>return Err(Failure::invalid())};owner.close(&r.producer_id,&r.reader,&r.session_id,r.session_seq,binding.as_ref())},
            _=>Err(Failure::invalid())
        }
    });
    match tokio::time::timeout(WAIT, job).await {
        Ok(Ok(Ok(value))) => crate::json_response(StatusCode::OK, &value),
        Ok(Ok(Err(error))) => error.response(),
        _ => Failure::unknown().response(),
    }
}
