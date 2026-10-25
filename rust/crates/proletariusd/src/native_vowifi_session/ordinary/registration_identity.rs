//! Successful registration metadata belongs to the captured reader/ICCID.
//! This updates host-side cache/notes only, never SIM files or IMS credentials.
use super::*;
use crate::{profile_notes, reader_reservation::Gate};
use std::sync::OnceLock;
use zeroize::Zeroizing;

struct Captured {
    reader: String,
    generation: u64,
    iccid: Zeroizing<String>,
    imsi: Zeroizing<String>,
    sim_phone: Zeroizing<String>,
    configured_identity: Zeroizing<String>,
}
impl Captured {
    fn current(&self, gate: &Gate, stop: &watch::Receiver<bool>) -> bool {
        !*stop.borrow()
            && stop.has_changed().is_ok()
            && gate.native_registered_identity_current(
                &self.reader,
                self.generation,
                &self.iccid,
                &self.imsi,
            )
    }
}
struct Observation {
    number: Option<String>,
    source: Option<&'static str>,
    error: Option<&'static str>,
}
fn pool() -> &'static crate::blocking_pool::BlockingPool {
    static POOL: OnceLock<crate::blocking_pool::BlockingPool> = OnceLock::new();
    POOL.get_or_init(|| crate::blocking_pool::BlockingPool::new(1))
}
fn nonmissing(number: &str) -> Option<String> {
    (!profile_notes::is_missing_phone(number)).then(|| number.trim().to_owned())
}
/// Called only after actual successful REGISTER, on the retained ordinary task.
/// A cancelled UI waiter does not own this worker or its file commit.
pub(super) async fn observe(
    context: &NativeSessionContext,
    session: &RegisteredImsSession,
    permit: &NativeSessionPermit,
    gate: Arc<Gate>,
    data_root: PathBuf,
    reporter: &Reporter,
) {
    if !permit.matches_key(&context.session_key) {
        return;
    }
    if context.card.kind() == card::SessionKind::Module {
        let actual = session
            .call_context()
            .ok()
            .and_then(|context| context.routing().registered_msisdn().and_then(nonmissing));
        let number = actual
            .clone()
            .or_else(|| nonmissing(&context.card.identity().phone_number));
        let source = if actual.is_some() {
            Some("associated_uri")
        } else {
            number.as_ref().map(|_| "sim")
        };
        let mut warning = None;
        if let Some(actual) = actual {
            let card = context.card.clone();
            let stop = permit.stop_signal();
            let iccid = context.card.identity().iccid.clone();
            let saved = pool()
                .run(move || {
                    profile_notes::Store::new(&data_root).remember_registered_phone(
                        &iccid,
                        &actual,
                        || !*stop.borrow() && stop.has_changed().is_ok() && card.identity_current(),
                    )
                })
                .await;
            if !matches!(saved, Ok(Ok(_))) {
                warning = Some("native_registered_phone_worker_unavailable");
            }
        }
        if context.card.identity_current() {
            reporter.registration_identity(number, source, warning);
        }
        return;
    }
    // Only the ordinary long-lived lease can update selected-reader metadata.
    // N-wait profile identity belongs to its frozen provider and catalog.
    let Some(lease) = context.card.ordinary_lease() else {
        return;
    };
    let captured = Captured {
        reader: context.reader.clone(),
        generation: context.reader_generation,
        iccid: context.card.identity().iccid.clone(),
        imsi: context.card.identity().imsi.clone(),
        sim_phone: context.card.identity().phone_number.clone(),
        configured_identity: Zeroizing::new(
            context.subscriber_identity.public_identity().to_owned(),
        ),
    };
    observe_bound(captured, session, permit, lease, gate, data_root, reporter).await;
}
fn registered_number(
    response: &proletarius_ims::SipResponse,
    configured: &str,
    fallback_routes: &[String],
) -> Option<String> {
    if !(200..300).contains(&response.status) {
        return None;
    }
    proletarius_vowifi::message_routing::RegisteredMessageRouting::from_response(
        response,
        configured,
        fallback_routes,
    )
    .ok()
    .and_then(|routing| routing.registered_msisdn().and_then(nonmissing))
}
async fn observe_bound(
    captured: Captured,
    session: &RegisteredImsSession,
    permit: &NativeSessionPermit,
    lease: &crate::reader_session_lease::NativeReaderSessionLease,
    gate: Arc<Gate>,
    data_root: PathBuf,
    reporter: &Reporter,
) {
    if !(200..300).contains(&session.final_response().status) {
        return;
    }
    let number = match session.call_context() {
        Ok(context) => context.routing().registered_msisdn().and_then(nonmissing),
        // Generic successful REGISTER sessions have no native-profile context.
        // Their fallback is the captured transport peer, never mutable UI/config.
        Err(_) => registered_number(
            session.final_response(),
            &captured.configured_identity,
            &[format!("<sip:{};lr>", session.signaling_peer())],
        ),
    };
    let stop = permit.stop_signal();
    if !captured.current(&gate, &stop) {
        return;
    }
    // Preserve successful-registration number evidence even when persistence
    // cannot be admitted. No fallback is labelled as P-Associated-URI evidence.
    let actual = number.clone();
    let sim_fallback = nonmissing(&captured.sim_phone);
    let result = lease
        .run_registration_metadata_worker(pool(), move || {
            commit(
                captured,
                gate,
                profile_notes::Store::new(&data_root),
                number,
                stop,
            )
        })
        .await;
    let observed = match result {
        Ok(value) => value,
        Err(_) => Observation {
            number: actual.clone().or(sim_fallback.clone()),
            source: if actual.is_some() {
                Some("associated_uri")
            } else {
                sim_fallback.as_ref().map(|_| "sim")
            },
            error: Some("native_registered_phone_worker_unavailable"),
        },
    };
    reporter.registration_identity(observed.number, observed.source, observed.error);
}
fn commit(
    captured: Captured,
    gate: Arc<Gate>,
    store: profile_notes::Store,
    actual: Option<String>,
    stop: watch::Receiver<bool>,
) -> Observation {
    if !captured.current(&gate, &stop) {
        return Observation {
            number: None,
            source: None,
            error: None,
        };
    }
    if let Some(number) = actual {
        let saved = store
            .remember_registered_phone(&captured.iccid, &number, || captured.current(&gate, &stop));
        // None means admission was rejected before any submission. An error
        // after submission is diagnostic, not proof that rename did not occur.
        if matches!(saved, Ok(None)) {
            return Observation {
                number: Some(number),
                source: Some("associated_uri"),
                error: None,
            };
        }
        if captured.current(&gate, &stop) || matches!(saved, Ok(Some(_))) {
            gate.native_registered_phone_update(
                &captured.reader,
                captured.generation,
                &captured.iccid,
                &captured.imsi,
                &number,
            );
        }
        return Observation {
            number: Some(number),
            source: Some("associated_uri"),
            error: saved.err(),
        };
    }
    if let Some(number) = nonmissing(&captured.sim_phone) {
        return Observation {
            number: Some(number),
            source: Some("sim"),
            error: None,
        };
    }
    match store.registered_phone_fallback(&captured.iccid) {
        Ok(value) => Observation {
            number: nonmissing(&value),
            source: nonmissing(&value).map(|_| "profile_note"),
            error: None,
        },
        Err(error) => Observation {
            number: None,
            source: None,
            error: Some(error),
        },
    }
}

#[cfg(test)]
mod tests;
