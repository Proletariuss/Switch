//! The durable SMS ledger stores a process- and session-bound capability.
//! Capturing a capability does not submit, claim, archive or replay a message.
use super::*;
use crate::native_registered_session::Control;
use crate::native_vowifi_supervisor::SessionGeneration;

#[derive(Clone)]
pub(crate) struct NativeSmsBinding {
    pub(crate) binding: String,
    pub(crate) subscription_id: String,
    pub(crate) generation: SessionGeneration,
    pub(crate) controls: tokio::sync::mpsc::Sender<Control>,
}

impl Service {
    pub(crate) fn sms_binding(&self) -> Result<NativeSmsBinding, &'static str> {
        let record = self.record()?.ok_or("native_sms_session_unavailable")?;
        let bound = self
            .current_binding(record.expected.reader_generation)
            .map_err(|_| "native_sms_session_unavailable")?;
        let handle = record
            .driver
            .handle()
            .ok_or("native_sms_session_unavailable")?;
        let snapshot = handle.snapshot();
        if !snapshot.sms_ready
            || snapshot.renewal_due
            || !snapshot.registered
            || snapshot.phase != ordinary::Phase::Registered
            || snapshot.binding.reader != record.expected.reader
            || snapshot.binding.reader_generation != record.expected.reader_generation
            || snapshot.binding.session_generation != bound.session_generation
            || handle.generation != bound.session_generation
            || !handle.controls.same_channel(&bound.controls)
            || !matches!(record.claim.try_is_current(), Ok(true))
            || *record.stop.borrow()
            || self.0.resources.identity.capture().ok().as_ref() != Some(&record.expected)
            || !self
                .record()?
                .is_some_and(|current| Arc::ptr_eq(&current, &record))
        {
            return Err("native_sms_binding_expired");
        }
        Ok(NativeSmsBinding {
            binding: capability(&self.0.sms_epoch, record.id, bound.session_generation),
            subscription_id: native_device_sms_subscription_id(
                &record.expected.iccid,
                &record.expected.imsi,
            )
            .ok_or("native_sms_subscription_unavailable")?,
            generation: bound.session_generation,
            controls: bound.controls,
        })
    }

    pub(crate) fn resolve_sms_binding(
        &self,
        binding: &str,
    ) -> Result<NativeSmsBinding, &'static str> {
        let captured = self.sms_binding()?;
        if captured.binding != binding {
            return Err("native_sms_binding_expired");
        }
        Ok(captured)
    }
}

fn capability(epoch: &str, intent_id: u64, generation: SessionGeneration) -> String {
    format!("{epoch}:{intent_id}:{}", generation.value())
}

// 128 random bits keep the complete ledger token bounded even at u64::MAX.
pub(super) fn new_epoch() -> Result<String, String> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy).map_err(|_| "native_sms_epoch_unavailable".to_owned())?;
    Ok(entropy.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests;
