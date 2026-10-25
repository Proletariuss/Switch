//! The existing durable device-send owner submits one typed native batch.
//! Native originating owns SIP/RP state; this layer alone archives the user's
//! outgoing message and consumes the existing durable claim.
use super::*;
use crate::{
    native_registered_session::{Control, Failure as ControlFailure},
    native_sms_originating::{Phase, Receipt, TextSend},
    native_vowifi_session::{NativeLifecycle, NativeSmsBinding},
};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

pub(super) trait ReaderSms: Send + Sync {
    fn has_intent(&self) -> bool;
    fn validate_business(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<(), &'static str>> + Send + '_>,
    > {
        Box::pin(async { Ok(()) })
    }
    fn capture(&self) -> std::result::Result<NativeSmsBinding, &'static str>;
    fn resolve(&self, token: &str) -> std::result::Result<NativeSmsBinding, &'static str>;
}
pub(super) struct RuntimeReaderSms(pub(super) Arc<NativeLifecycle>);
impl ReaderSms for RuntimeReaderSms {
    fn has_intent(&self) -> bool {
        self.0
            .ordinary_service()
            .is_some_and(|service| service.has_intent())
    }
    fn capture(&self) -> std::result::Result<NativeSmsBinding, &'static str> {
        self.0
            .ordinary_service()
            .ok_or("native_reader_sms_unavailable")?
            .sms_binding()
    }
    fn resolve(&self, token: &str) -> std::result::Result<NativeSmsBinding, &'static str> {
        self.0
            .ordinary_service()
            .ok_or("native_reader_sms_unavailable")?
            .resolve_sms_binding(token)
    }
}
pub(super) fn expired() -> Failure {
    coded(
        StatusCode::CONFLICT,
        "sms_intent_binding_expired",
        "原发送通道已变化或原生 IMS 短信尚未就绪，请核对后重新准备；没有重放这条短信",
    )
}
fn receipt_valid(receipt: &Receipt, route: &NativeSmsBinding, batch: &str) -> bool {
    receipt.generation == route.generation
        && receipt.batch_id == batch
        && receipt.total_parts > 0
        && receipt.total_parts <= 1024
        && receipt.accepted_parts <= receipt.total_parts
}
fn terminal(receipt: &Receipt) -> bool {
    !matches!(receipt.phase, Phase::Prepared | Phase::AwaitingResponse)
}
fn result(receipt: &Receipt) -> Value {
    json!({
        "accepted": receipt.phase == Phase::Accepted
            && receipt.accepted_parts == receipt.total_parts
            && !receipt.outcome_unknown(),
        "segments": receipt.accepted_parts,
    })
}
async fn reply<T>(
    controls: &tokio::sync::mpsc::Sender<Control>,
    command: Control,
    receiver: oneshot::Receiver<std::result::Result<T, ControlFailure>>,
    timeout: Duration,
) -> std::result::Result<T, ()> {
    tokio::time::timeout(timeout, async {
        controls.send(command).await.map_err(|_| ())?;
        receiver.await.map_err(|_| ())?.map_err(|_| ())
    })
    .await
    .map_err(|_| ())?
}
impl Service {
    pub(super) async fn execute_native_ims(&self, record: &Record, attempt: &str) -> Result<Value> {
        let (owner, token) =
            if let Some(token) = record.binding.strip_prefix(RUST_MODULE_IMS_BINDING_PREFIX) {
                (&self.native_module_sms, token)
            } else {
                (
                    &self.native_sms,
                    record
                        .binding
                        .strip_prefix(RUST_READER_IMS_BINDING_PREFIX)
                        .ok_or_else(unknown)?,
                )
            };
        // Durable permission already committed. Validate the frozen route on
        // both sides of the asynchronous module profile/CFUN4 check: a new
        // session cannot inherit an old intent while physical validation waits.
        let before = owner.resolve(token).map_err(|_| unknown())?;
        owner.validate_business().await.map_err(|_| unknown())?;
        let route = owner.resolve(token).map_err(|_| unknown())?;
        if route.binding != token
            || route.subscription_id != record.subscription_id
            || before.generation != route.generation
            || before.subscription_id != route.subscription_id
            || !before.controls.same_channel(&route.controls)
        {
            return Err(unknown());
        }
        // The existing claim is unique and consumed once. Never use contents
        // as deduplication or re-submit SendText after a lost acknowledgement.
        let batch = format!("device_sms_{attempt}");
        let (sender, receiver) = oneshot::channel();
        let command = Control::SendText {
            input: TextSend {
                generation: route.generation,
                batch_id: batch.clone(),
                recipient: Zeroizing::new(record.phone.clone()),
                text: Zeroizing::new(record.message.clone()),
                request_status_report: true,
            },
            reply: sender,
        };
        let mut receipt = reply(&route.controls, command, receiver, self.io_timeout)
            .await
            .map_err(|_| unknown())?;
        if !receipt_valid(&receipt, &route, &batch) {
            return Err(unknown());
        }
        let deadline = tokio::time::Instant::now() + EXECUTE_TIMEOUT;
        while !terminal(&receipt) {
            #[cfg(test)]
            ShutdownTestGate::checkpoint(&self.shutdown_hooks.ims_poll).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            let (sender, receiver) = oneshot::channel();
            let command = Control::SmsStatus {
                generation: route.generation,
                batch_id: batch.clone(),
                reply: sender,
            };
            let next = tokio::time::timeout_at(
                deadline,
                reply(&route.controls, command, receiver, self.io_timeout),
            )
            .await;
            match next {
                Ok(Ok(Some(next)))
                    if receipt_valid(&next, &route, &batch)
                        && next.total_parts == receipt.total_parts
                        && next.accepted_parts >= receipt.accepted_parts =>
                {
                    receipt = next
                }
                _ => return Ok(json!({"accepted":false,"segments":receipt.accepted_parts})),
            }
        }
        // A result still belongs to its frozen SIM even if a later session is
        // already published. Archive it with the durable captured subscription,
        // never send it again or attribute it to the replacement session.
        Ok(result(&receipt))
    }
}

#[cfg(test)]
mod tests;
