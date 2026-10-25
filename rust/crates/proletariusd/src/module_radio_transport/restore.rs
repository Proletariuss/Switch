//! Durable restore-write receipts at the shared physical Plan boundary.
use super::*;
use crate::call_mode_restore_journal::{Mutation, MutationKind, PreparedWrite, Readback};

pub(super) const ERROR: &str = "call_mode_restore_journal_unavailable";
#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Prepared,
    Submitting,
    Acknowledged,
}
pub(super) struct PendingWrite {
    pub(super) mutation: Mutation,
    pub(super) prepared: PreparedWrite,
    phase: Phase,
}
impl Plan {
    pub(crate) fn restore_kind(&self) -> Option<MutationKind> {
        match (self.stage, &self.action) {
            (Stage::VoiceSetVoLTE, Action::CallModeVoice { target }) => Some(MutationKind::Volte {
                disabled: target.volte_disabled,
            }),
            (Stage::VoiceSetIMS, Action::CallModeVoice { target }) => {
                Some(MutationKind::Ims { target: target.ims })
            }
            (
                Stage::CallModeUsbCompositionWrite,
                Action::CallModeUsbCompositionWrite { expected, target },
            ) => Some(MutationKind::UsbComposition {
                expected: *expected,
                target: *target,
            }),
            (Stage::Reboot, Action::Reboot) => Some(MutationKind::Reboot),
            (Stage::CallModeAdbAuthorizeSubmit, Action::CallModeAdbAuthorize) => {
                Some(MutationKind::AdbAuthorize)
            }
            _ => None,
        }
    }
    pub(crate) fn prepare_commit_before_write(
        &mut self,
        step: &Step,
        binding: &Binding,
    ) -> Result<(), &'static str> {
        self.validate_authority_before_write(step, binding)?;
        if !step.write {
            return Ok(());
        }
        let Some(kind) = self.restore_kind() else {
            return Ok(());
        };
        let Some(proof) = self.call_mode_commit.clone() else {
            return self.validate_commit_before_write(step, binding);
        };
        if self.restore_write.is_some() {
            return Err(ERROR);
        }
        if let Some((mutation, prepared)) = proof.prepare_restore_write(kind).map_err(|_| ERROR)? {
            self.restore_write = Some(PendingWrite {
                mutation,
                prepared,
                phase: Phase::Prepared,
            });
        }
        self.validate_commit_before_write(step, binding)
    }
    pub(super) fn validate_restore_before_write(&self, step: &Step) -> Result<(), &'static str> {
        if !step.write {
            return Ok(());
        }
        let Some(proof) = self.call_mode_commit.as_ref() else {
            return Ok(());
        };
        if proof.restore_journal().map_err(|_| ERROR)?.is_none() {
            return Ok(());
        }
        let pending = self.restore_write.as_ref().ok_or(ERROR)?;
        if self.restore_kind().as_ref() != Some(&pending.mutation.kind) {
            return Err(ERROR);
        }
        pending
            .prepared
            .validate_for_write(&pending.mutation)
            .map_err(|error| error.code())
    }
    // Local observation only: the physical boundary may now submit bytes.
    // Durable Prepared already exists; an unwind must conservatively record Unknown.
    pub(crate) fn note_write_started(&mut self) {
        if let Some(pending) = self.restore_write.as_mut() {
            pending.phase = Phase::Submitting;
        }
    }
    pub(crate) fn record_write_not_submitted(&mut self) -> Result<(), &'static str> {
        if let Some(pending) = self.restore_write.take() {
            pending.prepared.record_not_submitted().map_err(|_| ERROR)?;
        }
        Ok(())
    }
    pub(super) fn finish_restore_on_failure(&mut self) -> Result<(), &'static str> {
        let Some(pending) = self.restore_write.take() else {
            return Ok(());
        };
        let result = if pending.phase == Phase::Prepared {
            pending.prepared.record_not_submitted()
        } else {
            pending.prepared.record_submitted_unknown()
        };
        result.map_err(|_| ERROR)
    }
    pub(super) fn accept_restore_reply(&mut self, reply: &Reply) -> Result<(), &'static str> {
        let is_write = self.restore_kind().is_some();
        let Some(pending) = self.restore_write.as_mut() else {
            return Ok(());
        };
        if is_write {
            if reply.uncertain || reply.fatal {
                let pending = self.restore_write.take().unwrap();
                return pending
                    .prepared
                    .record_submitted_unknown()
                    .map_err(|_| ERROR);
            }
            if !voice_write_accepted(reply) {
                let pending = self.restore_write.take().unwrap();
                return pending.prepared.record_known_rejected().map_err(|_| ERROR);
            }
            pending.prepared.record_acknowledged().map_err(|_| ERROR)?;
            pending.phase = Phase::Acknowledged;
            if matches!(
                pending.mutation.kind,
                MutationKind::Reboot | MutationKind::AdbAuthorize
            ) {
                // Reboot is retained by its exact reservation for actual
                // successor verification. ADB ACK alone remains pending.
                self.restore_write = None;
            }
            return Ok(());
        }
        let readback = if reply.ok && !reply.uncertain && !reply.fatal {
            match (&pending.mutation.kind, self.stage) {
                (
                    MutationKind::Volte { .. },
                    Stage::VoiceVerifyVoLTEBeforeIMS | Stage::VoiceReadBackVoLTE,
                ) => parse_voice_volte_disabled(&reply.text)
                    .map(|disabled| Readback::Volte { disabled }),
                (MutationKind::Ims { .. }, Stage::VoiceReadBackIMS) => parse_voice_ims(&reply.text)
                    .map(|(ims, volte_capability)| Readback::Ims {
                        ims,
                        volte_capability,
                    }),
                (
                    MutationKind::UsbComposition { expected, target },
                    Stage::CallModeUsbCompositionReadBack,
                ) => parse_usb_composition(&reply.text)
                    .filter(|actual| usb_readback_error(*expected, *target, *actual).is_none())
                    .map(|actual| Readback::UsbComposition { actual }),
                _ => None,
            }
        } else {
            None
        };
        if let Some(readback) = readback {
            let pending = self.restore_write.take().unwrap();
            pending
                .prepared
                .record_verified_readback(readback)
                .map_err(|_| ERROR)?;
        }
        Ok(())
    }
}
impl Drop for Plan {
    fn drop(&mut self) {
        let _ = self.finish_restore_on_failure();
    }
}
