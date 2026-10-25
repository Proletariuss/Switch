//! Fixed RF command plans shared by the USB owner and the serial actor.
//! Policy, durable preferences and long-lived radio intents live elsewhere.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    modem_actor::ModemActor,
    usb_at::{Failure, Owner},
};
mod restore;

pub(crate) const PLAN_BUDGET: Duration = Duration::from_secs(55);
pub(crate) const RESPONSE_LIMIT: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub(crate) enum Binding {
    Usb {
        producer_id: String,
        session_id: String,
    },
    Serial {
        serial_session: u64,
    },
}
#[derive(Clone, Debug)]
pub(crate) enum Action {
    CellularCallRead,
    CellularCall(CallCommand),
    Observe {
        cellular: bool,
    },
    SetVerify {
        target: u8,
    },
    RefreshAfterProfileSwitch {
        target: u8,
    },
    Reboot,
    Diagnostic {
        command: String,
        timeout_ms: u64,
    },
    /// Fixed, bounded operations over the module's ME phonebook. Public
    /// callers cannot provide raw AT commands through this action.
    ProfileNotes(ProfileNoteCommand),
    /// Reboot-effective QCFG transaction, admitted only by the call-mode lease.
    CallModeVoice {
        target: VoiceConfiguration,
    },
    /// Read-only IMS/VoLTE tuple query, admitted by call-mode or call leases.
    CallModeVoiceRead,
    /// Fixed identity proof; ATI is advisory after a strict CGSN response.
    CallModeIdentityRead,
    CallModeIdleRead,
    /// Read-only, schema-validated Quectel USB composition query.
    CallModeUsbCompositionRead,
    /// Read-only probe that confirms whether the legacy ADB challenge is supported.
    CallModeAdbProbe,
    /// Confirm the call state, derive the legacy challenge response, and submit it once.
    CallModeAdbAuthorize,
    /// Compare-and-write a known USB composition, then verify its readback.
    CallModeUsbCompositionWrite {
        expected: UsbComposition,
        target: UsbComposition,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProfileNoteCommand {
    TestStorage,
    SelectME,
    Status,
    ReadSupport,
    WriteSupport,
    Read { first: usize, last: usize },
    Write { index: usize, record: String },
    Delete { index: usize },
}
impl ProfileNoteCommand {
    fn mutating(&self) -> bool {
        matches!(
            self,
            Self::SelectME | Self::Write { .. } | Self::Delete { .. }
        )
    }

    fn command(&self) -> Result<(String, Duration), Failure> {
        match self {
            Self::TestStorage => Ok(("AT+CPBS=?".into(), Duration::from_secs(6))),
            Self::SelectME => Ok((r#"AT+CPBS="ME""#.into(), Duration::from_secs(6))),
            Self::Status => Ok(("AT+CPBS?".into(), Duration::from_secs(6))),
            Self::ReadSupport => Ok(("AT+CPBR=?".into(), Duration::from_secs(6))),
            Self::WriteSupport => Ok(("AT+CPBW=?".into(), Duration::from_secs(6))),
            Self::Read { first, last } if *first > 0 && first <= last && *last <= 65_535 => {
                Ok((format!("AT+CPBR={first},{last}"), Duration::from_secs(20)))
            }
            Self::Write { index, record }
                if *index > 0
                    && *index <= 65_535
                    && record.len() <= 255
                    && !record.is_empty()
                    && record
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_|-".contains(&byte)) =>
            {
                Ok((
                    format!(r#"AT+CPBW={index},"00000000000",129,"{record}""#),
                    Duration::from_secs(8),
                ))
            }
            Self::Delete { index } if *index > 0 && *index <= 65_535 => {
                Ok((format!("AT+CPBW={index}"), Duration::from_secs(8)))
            }
            _ => Err(crate::module_radio_transport::failure(
                "module_profile_notes_invalid_command",
            )),
        }
    }
}
impl Action {
    /// These typed configuration writes may only reach physical I/O with the
    /// original call-mode owner's sealed commit proof. Reboot is also used by
    /// non-call-mode owners; its scope is determined by the accepting service.
    pub(crate) fn requires_call_mode_commit(&self) -> bool {
        matches!(
            self,
            Self::CallModeVoice { .. }
                | Self::CallModeUsbCompositionWrite { .. }
                | Self::CallModeAdbAuthorize
        )
    }
    pub(crate) fn mutating(&self) -> bool {
        match self {
            Self::ProfileNotes(command) => command.mutating(),
            Self::Observe { .. }
            | Self::CellularCallRead
            | Self::CallModeIdleRead
            | Self::CallModeIdentityRead
            | Self::CallModeVoiceRead
            | Self::CallModeUsbCompositionRead
            | Self::CallModeAdbProbe => false,
            _ => true,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), Failure> {
        if let Self::CellularCall(command) = self {
            command.command()?;
        }
        if matches!(self, Self::Diagnostic { command, timeout_ms } if command.is_empty() || command.len() > 4096 || command.chars().any(char::is_control) || !(1..=20_000).contains(timeout_ms))
        {
            return Err(failure("radio_invalid_diagnostic"));
        }
        if matches!(self, Self::ProfileNotes(command) if command.command().is_err()) {
            Err(failure("module_profile_notes_invalid_command"))
        } else if matches!(self, Self::SetVerify { target } | Self::RefreshAfterProfileSwitch { target } if *target > 4)
        {
            Err(failure("radio_invalid_mode"))
        } else if matches!(self, Self::CallModeVoice { target } if !target.valid()) {
            Err(failure("call_mode_voice_target_invalid"))
        } else if matches!(self, Self::CallModeUsbCompositionWrite { expected, target } if !expected.valid() || !target.valid() || expected.vendor_id != target.vendor_id || expected.product_id != target.product_id)
        {
            Err(failure("call_mode_usb_target_invalid"))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CallModeModuleIdentity {
    pub(crate) imei: String,
    pub(crate) firmware: String,
}
/// Fixed IMS/VoLTE tuple. Only IMS and `volte_disabled` are writable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VoiceConfiguration {
    pub(crate) ims: u8,
    pub(crate) volte_capability: u8,
    pub(crate) volte_disabled: u8,
}
/// Validated Quectel USBCFG response. A fixed array makes malformed feature
/// counts unrepresentable after parsing.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsbComposition {
    pub(crate) vendor_id: u16,
    pub(crate) product_id: u16,
    pub(crate) flags: [u8; 7],
}
impl VoiceConfiguration {
    pub(crate) fn valid(self) -> bool {
        self.ims <= 2 && self.volte_capability <= 1 && self.volte_disabled <= 1
    }
}
impl UsbComposition {
    pub(crate) fn valid(self) -> bool {
        self.vendor_id > 0 && self.product_id > 0 && self.flags.iter().all(|flag| *flag <= 1)
    }

    pub(crate) fn call_mode_target(self) -> Self {
        let mut target = self;
        target.flags[5] = 1;
        target.flags[6] = 1;
        target
    }
}
#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct Outcome {
    pub(crate) complete: bool,
    pub(crate) submitted: bool,
    pub(crate) verified: bool,
    pub(crate) uncertain: bool,
    pub(crate) restore_confirmed: bool,
    pub(crate) cfun: Option<u8>,
    pub(crate) cereg: Option<u8>,
    pub(crate) creg: Option<u8>,
    pub(crate) signal: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) voice: Option<VoiceConfiguration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) module_identity: Option<CallModeModuleIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) usb_composition: Option<UsbComposition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) usb_composition_target: Option<UsbComposition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) adb_authorization_supported: Option<bool>,
    pub(crate) error_code: String,
    pub(crate) response: String,
}
impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outcome")
            .field("complete", &self.complete)
            .field("submitted", &self.submitted)
            .field("verified", &self.verified)
            .field("uncertain", &self.uncertain)
            .field("voice", &self.voice)
            .field("usb_composition", &self.usb_composition)
            .field("usb_composition_target", &self.usb_composition_target)
            .field(
                "adb_authorization_supported",
                &self.adb_authorization_supported,
            )
            .field("error_code", &self.error_code)
            .finish_non_exhaustive()
    }
}
pub(crate) fn failure(code: &'static str) -> Failure {
    Failure::new(
        StatusCode::CONFLICT,
        code,
        "Module radio operation was not confirmed",
    )
}
pub(crate) async fn capture_serial_binding(modem: &ModemActor) -> Result<Binding, Failure> {
    modem
        .radio_binding()
        .await
        .map(|serial_session| Binding::Serial { serial_session })
}
pub(crate) async fn execute_with_budget(
    owner: &Owner,
    modem: &ModemActor,
    binding: Binding,
    action: Action,
    permitted_inhibit: Option<String>,
    budget: Duration,
    cancelled: Arc<AtomicBool>,
) -> Result<Outcome, Failure> {
    action.validate()?;
    if budget.is_zero() {
        return Err(failure("radio_plan_timeout"));
    }
    if action.requires_call_mode_commit() {
        return Err(failure("call_mode_operation_lock_unavailable"));
    }
    match binding {
        Binding::Usb {
            producer_id,
            session_id,
        } => {
            owner
                .radio_execute_with_budget(
                    producer_id,
                    session_id,
                    action,
                    permitted_inhibit,
                    budget,
                    cancelled,
                )
                .await
        }
        Binding::Serial { serial_session } => {
            if modem.native_serial_managed()
                && let Some(token) = permitted_inhibit
            {
                return modem
                    .radio_execute_module_with_budget(
                        serial_session,
                        &token,
                        action,
                        budget,
                        cancelled,
                    )
                    .await;
            }
            modem
                .radio_execute_with_budget(serial_session, action, budget, cancelled)
                .await
        }
    }
}

/// Dispatch one finite operation accepted by its radio owner. Both transport
/// actors validate this unforgeable right alongside their original binding.
pub(crate) async fn execute_with_budget_and_right(
    owner: &Owner,
    modem: &ModemActor,
    binding: Binding,
    action: Action,
    permitted_inhibit: Option<String>,
    budget: Duration,
    cancelled: Arc<AtomicBool>,
    right: crate::module_recovery::OperationRight,
    producer: String,
    commit: Option<crate::module_radio::CallModeCommitProof>,
) -> Result<Outcome, Failure> {
    action.validate()?;
    if budget.is_zero() {
        return Err(failure("radio_plan_timeout"));
    }
    if let Some(proof) = &commit {
        proof
            .validate_for_binding(&action, &binding)
            .map_err(|error| Failure {
                status: error.status,
                code: error.code,
                message: error.message,
                response: String::new(),
                uncertain: false,
            })?;
    } else if action.requires_call_mode_commit() {
        return Err(failure("call_mode_operation_lock_unavailable"));
    }
    match binding {
        Binding::Usb {
            producer_id,
            session_id,
        } => {
            owner
                .radio_execute_with_budget_and_right(
                    producer_id,
                    session_id,
                    action,
                    permitted_inhibit,
                    budget,
                    cancelled,
                    right,
                    commit,
                )
                .await
        }
        Binding::Serial { serial_session } => {
            modem
                .radio_execute_with_right(
                    serial_session,
                    permitted_inhibit,
                    action,
                    budget,
                    cancelled,
                    right,
                    producer,
                    commit,
                )
                .await
        }
    }
}

/// Classify writes at the raw compatibility boundary, including Hayes command
/// concatenation. Quoted values are data; CFUN? and CFUN=? remain queries.
pub(crate) fn is_radio_write(command: &str) -> bool {
    // Modems disagree whether a backslash escapes a quote. Reject a write if
    // either interpretation sees one outside data, matching console policy.
    radio_write_with_quotes(command, true) || radio_write_with_quotes(command, false)
}
pub(crate) fn is_cellular_probe(command: &str) -> bool {
    [true, false].into_iter().any(|escapes| {
        let text = unquoted_command(command, escapes);
        [
            "+CSQ", "+CESQ", "+QCSQ", "+CEREG?", "+CEREG=?", "+CREG?", "+CREG=?", "+CGREG?",
            "+CGREG=?", "+COPS?", "+COPS=", "+QNWINFO", "+QENG", "+QSCAN", "+CGATT=1",
        ]
        .iter()
        .any(|pattern| text.contains(pattern))
    })
}
fn radio_write_with_quotes(command: &str, escapes: bool) -> bool {
    let text = unquoted_command(command, escapes);
    let mut rest = text.as_str();
    while let Some(at) = rest.find("+CFUN=") {
        rest = &rest[at + 6..];
        if !rest.starts_with('?') {
            return true;
        }
    }
    false
}
fn unquoted_command(command: &str, escapes: bool) -> String {
    let mut text = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in command.chars() {
        if quoted {
            if escaped {
                escaped = false;
                continue;
            }
            if escapes && c == '\\' {
                escaped = true;
                continue;
            }
            if c == '"' {
                quoted = false;
            }
            continue;
        }
        if c == '"' {
            quoted = true;
            text.push(' ');
        } else if !c.is_ascii_whitespace() {
            text.push(c.to_ascii_uppercase());
        }
    }
    text
}

pub(crate) struct Step {
    pub(crate) command: String,
    pub(crate) timeout: Duration,
    pub(crate) write: bool,
    pub(crate) sensitive: bool,
}
pub(crate) struct Reply {
    pub(crate) text: String,
    pub(crate) ok: bool,
    pub(crate) uncertain: bool,
    pub(crate) fatal: bool,
    pub(crate) code: String,
}
impl Reply {
    pub(crate) fn response(text: String) -> Self {
        let terminal = text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .next_back()
            .unwrap_or("");
        let ok = terminal == "OK";
        let rejected = terminal == "ERROR"
            || terminal.starts_with("+CME ERROR:")
            || terminal.starts_with("+CMS ERROR:");
        Self {
            text,
            ok,
            uncertain: !ok && !rejected,
            fatal: false,
            code: if ok {
                ""
            } else if rejected {
                "radio_command_rejected"
            } else {
                "radio_response_incomplete"
            }
            .into(),
        }
    }
    pub(crate) fn error(error: &Failure) -> Self {
        Self {
            text: String::new(),
            ok: false,
            uncertain: error.uncertain,
            fatal: error.code != "usb_at_timeout",
            code: error.code.into(),
        }
    }
}
#[derive(Clone, Copy)]
enum After {
    Finish,
    RefreshGap,
}
#[derive(Clone, Copy)]
enum Stage {
    Initial,
    Calls {
        mode: u8,
        refresh_first: bool,
    },
    Set {
        mode: u8,
        refresh_first: bool,
    },
    Poll {
        target: u8,
        until: Instant,
        after: After,
    },
    Cereg,
    Creg,
    Signal,
    RebootCalls,
    Reboot,
    DiagnosticCalls,
    Diagnostic,
    ProfileNotesCalls,
    ProfileNotesCommand,
    CallModeIdleCalls,
    CellularCallRead,
    CellularDialCalls,
    CellularCallCommand,
    CallModeIdentityIMEI,
    CallModeIdentityFirmware,
    CallModeVoiceCalls,
    CallModeUsbCompositionRead,
    CallModeUsbCompositionWriteCalls,
    CallModeUsbCompositionReadBeforeWrite,
    CallModeUsbCompositionWrite,
    CallModeUsbCompositionReadBack,
    CallModeAdbProbe,
    CallModeAdbAuthorizeCalls,
    CallModeAdbAuthorizeChallenge,
    CallModeAdbAuthorizeSubmit,
    VoiceReadIMS,
    VoiceReadVoLTE,
    VoiceSetVoLTE,
    VoiceSetIMS,
    VoiceVerifyVoLTEBeforeIMS,
    VoiceReadBackIMS,
    VoiceReadBackVoLTE,
    Done,
}
pub(crate) struct Plan {
    action: Action,
    stage: Stage,
    ready_at: Instant,
    deadline: Instant,
    outcome: Outcome,
    voice_current: Option<VoiceConfiguration>,
    adb_challenge: Option<Zeroizing<String>>,
    retire: bool,
    cancelled: Arc<AtomicBool>,
    call_mode_commit: Option<crate::module_radio::CallModeCommitProof>,
    restore_write: Option<restore::PendingWrite>,
}
impl Plan {
    #[cfg(test)]
    pub(crate) fn new(action: Action) -> Self {
        Self::new_with_budget(action, PLAN_BUDGET)
    }
    #[cfg(test)]
    pub(crate) fn new_with_budget(action: Action, budget: Duration) -> Self {
        Self::new_with_cancelled(action, budget, Arc::new(AtomicBool::new(false)))
    }
    pub(crate) fn new_with_cancelled(
        action: Action,
        budget: Duration,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        let now = Instant::now();
        let stage = match &action {
            Action::CellularCallRead => Stage::CellularCallRead,
            Action::CellularCall(command)
                if command.action == crate::module_radio::CallAction::Dial =>
            {
                Stage::CellularDialCalls
            }
            Action::CellularCall(_) => Stage::CellularCallCommand,
            Action::Reboot => Stage::RebootCalls,
            Action::Diagnostic { .. } => Stage::DiagnosticCalls,
            Action::ProfileNotes(_) => Stage::ProfileNotesCalls,
            Action::CallModeVoice { .. } => Stage::CallModeVoiceCalls,
            Action::CallModeVoiceRead => Stage::VoiceReadIMS,
            Action::CallModeIdentityRead => Stage::CallModeIdentityIMEI,
            Action::CallModeIdleRead => Stage::CallModeIdleCalls,
            Action::CallModeUsbCompositionRead => Stage::CallModeUsbCompositionRead,
            Action::CallModeUsbCompositionWrite { .. } => Stage::CallModeUsbCompositionWriteCalls,
            Action::CallModeAdbProbe => Stage::CallModeAdbProbe,
            Action::CallModeAdbAuthorize => Stage::CallModeAdbAuthorizeCalls,
            _ => Stage::Initial,
        };
        Self {
            action,
            stage,
            ready_at: now,
            deadline: now + budget.min(PLAN_BUDGET),
            outcome: Outcome::default(),
            voice_current: None,
            adb_challenge: None,
            retire: false,
            cancelled,
            call_mode_commit: None,
            restore_write: None,
        }
    }
    pub(crate) fn set_call_mode_commit_proof(
        &mut self,
        proof: Option<crate::module_radio::CallModeCommitProof>,
    ) {
        self.call_mode_commit = proof;
    }

    /// The actor calls this for its actual binding immediately before sending
    /// a write. Readback and exact accepted cleanup retain their old authority.
    pub(crate) fn validate_commit_before_write(
        &self,
        step: &Step,
        binding: &Binding,
    ) -> Result<(), &'static str> {
        self.validate_authority_before_write(step, binding)?;
        self.validate_restore_before_write(step)
    }
    fn validate_authority_before_write(
        &self,
        step: &Step,
        binding: &Binding,
    ) -> Result<(), &'static str> {
        if !step.write {
            return Ok(());
        }
        if self.cancelled.load(Ordering::Acquire) {
            return Err("radio_owner_revoked");
        }
        if let Action::CellularCall(command) = &self.action
            && matches!(self.stage, Stage::CellularCallCommand)
            && !(command.admission)()
        {
            return Err("module_call_admission_changed");
        }
        match &self.call_mode_commit {
            Some(proof) => proof
                .validate_for_binding(&self.action, binding)
                .map_err(|error| error.code),
            None if self.action.requires_call_mode_commit() => {
                Err("call_mode_operation_lock_unavailable")
            }
            None => Ok(()),
        }
    }

    /// The USB transport rechecks this after draining old input and directly
    /// before its actual I/O. This callback is derived from a sealed owner
    /// proof; a caller-supplied predicate cannot mint a call-mode capability.
    pub(crate) fn admission_guard_for_step(
        &self,
        step: &Step,
        binding: Binding,
    ) -> crate::module_radio::CallAdmission {
        let cancelled = self.cancelled.clone();
        let admission = match &self.action {
            Action::CellularCall(command) if matches!(self.stage, Stage::CellularCallCommand) => {
                Some(command.admission.clone())
            }
            _ => None,
        };
        let proof = step.write.then(|| self.call_mode_commit.clone()).flatten();
        let required = step.write && self.action.requires_call_mode_commit();
        let action = self.action.clone();
        let prepared = self
            .restore_write
            .as_ref()
            .map(|write| (write.mutation.clone(), write.prepared.clone()));
        Arc::new(move || {
            !cancelled.load(Ordering::Acquire)
                && admission.as_ref().is_none_or(|admission| admission())
                && match &proof {
                    Some(proof) => {
                        proof.validate_for_binding(&action, &binding).is_ok()
                            && match proof.restore_journal() {
                                Ok(Some(_)) => {
                                    prepared.as_ref().is_some_and(|(mutation, token)| {
                                        token.validate_for_write(mutation).is_ok()
                                    })
                                }
                                Ok(None) => true,
                                Err(_) => false,
                            }
                    }
                    None => !required,
                }
        })
    }
    // Pure observation for the separate startup reconciliation driver. This
    // neither supplies nor bypasses the ordinary actor's commit authority.
    pub(crate) fn current_voice_observation(&self) -> Option<VoiceConfiguration> {
        self.voice_current
    }
    pub(crate) fn mark_submitted(&mut self) {
        self.outcome.submitted = true;
    }
    pub(crate) fn unconfirmed_write(&self) -> bool {
        self.outcome.submitted && !self.outcome.verified
    }
    pub(crate) fn ready_at(&self) -> Instant {
        self.ready_at.min(self.deadline)
    }
    pub(crate) fn done(&self) -> bool {
        matches!(self.stage, Stage::Done)
    }
    pub(crate) fn mutating(&self) -> bool {
        self.action.mutating()
    }
    pub(crate) fn cellular_call(&self) -> bool {
        matches!(self.action, Action::CellularCall(_))
    }
    pub(crate) fn retire_transport(&self) -> bool {
        self.retire
    }
    pub(crate) fn sensitive_in_flight(&self) -> bool {
        matches!(self.stage, Stage::CallModeAdbAuthorizeSubmit)
    }
    pub(crate) fn finish(self) -> Outcome {
        let mut this = self;
        std::mem::take(&mut this.outcome)
    }
    pub(crate) fn fail(&mut self, code: &str, uncertain: bool) {
        let journal_failed = self.finish_restore_on_failure().is_err();
        self.outcome.complete = false;
        self.outcome.uncertain |= uncertain || journal_failed && self.outcome.submitted;
        self.outcome.error_code = if journal_failed { restore::ERROR } else { code }.into();
        self.stage = Stage::Done;
    }
    pub(crate) fn next(&mut self) -> Option<Step> {
        if self.cancelled.load(Ordering::Acquire) {
            self.fail("radio_owner_revoked", self.unconfirmed_write());
            return None;
        }
        let now = Instant::now();
        if self.done() {
            return None;
        }
        if now >= self.deadline {
            self.fail(
                "radio_plan_timeout",
                self.outcome.submitted && !self.outcome.verified,
            );
            return None;
        }
        if now < self.ready_at {
            return None;
        }
        if self.sensitive_in_flight() {
            let challenge = self
                .adb_challenge
                .as_ref()
                .map(|value| value.as_str())
                .unwrap_or_default();
            let Some(password) = crate::qadbkey::password(challenge) else {
                self.fail("call_mode_adb_password_failed", false);
                return None;
            };
            let mut command = Zeroizing::new(format!(r#"AT+QADBKEY="{}""#, password.as_str()));
            return Some(Step {
                command: std::mem::take(&mut *command),
                timeout: Duration::from_secs(8).min(self.deadline.saturating_duration_since(now)),
                write: true,
                sensitive: true,
            });
        }
        let (command, seconds, write) = match self.stage {
            Stage::Initial | Stage::Poll { .. } => ("AT+CFUN?".into(), 4, false),
            Stage::Calls { .. }
            | Stage::RebootCalls
            | Stage::CellularCallRead
            | Stage::CellularDialCalls
            | Stage::CallModeIdleCalls
            | Stage::DiagnosticCalls
            | Stage::ProfileNotesCalls
            | Stage::CallModeVoiceCalls
            | Stage::CallModeAdbAuthorizeCalls => ("AT+CLCC".into(), 4, false),
            Stage::CallModeIdentityIMEI => ("AT+CGSN".into(), 4, false),
            Stage::CallModeIdentityFirmware => ("ATI".into(), 4, false),
            Stage::CallModeAdbProbe | Stage::CallModeAdbAuthorizeChallenge => {
                ("AT+QADBKEY?".into(), 4, false)
            }
            Stage::CallModeAdbAuthorizeSubmit => unreachable!("handled before command selection"),
            Stage::CallModeUsbCompositionRead => (r#"AT+QCFG="USBCFG""#.into(), 5, false),
            Stage::CallModeUsbCompositionWriteCalls => ("AT+CLCC".into(), 4, false),
            Stage::CallModeUsbCompositionReadBeforeWrite
            | Stage::CallModeUsbCompositionReadBack => (r#"AT+QCFG="USBCFG""#.into(), 5, false),
            Stage::CallModeUsbCompositionWrite => {
                let Action::CallModeUsbCompositionWrite { target, .. } = self.action else {
                    unreachable!()
                };
                (usb_composition_command(target), 8, true)
            }
            Stage::Set {
                mode,
                refresh_first,
            } => (
                format!("AT+CFUN={mode}"),
                if refresh_first {
                    5
                } else if matches!(self.action, Action::RefreshAfterProfileSwitch { .. }) {
                    8
                } else {
                    12
                },
                true,
            ),
            Stage::Cereg => ("AT+CEREG?".into(), 4, false),
            Stage::Creg => ("AT+CREG?".into(), 4, false),
            Stage::Signal => ("AT+CSQ".into(), 4, false),
            Stage::Reboot => ("AT+CFUN=1,1".into(), 5, true),
            Stage::CellularCallCommand => {
                let Action::CellularCall(ref command) = self.action else {
                    unreachable!()
                };
                if !(command.admission)() {
                    self.fail("module_call_admission_changed", false);
                    return None;
                }
                let (command, timeout) = command.command().expect("validated fixed call command");
                return Some(Step {
                    command,
                    timeout: timeout.min(self.deadline.saturating_duration_since(now)),
                    write: true,
                    sensitive: false,
                });
            }
            Stage::Diagnostic => {
                let Action::Diagnostic {
                    ref command,
                    timeout_ms,
                } = self.action
                else {
                    unreachable!()
                };
                return Some(Step {
                    command: command.clone(),
                    timeout: Duration::from_millis(timeout_ms)
                        .min(self.deadline.saturating_duration_since(now)),
                    write: true,
                    sensitive: false,
                });
            }
            Stage::ProfileNotesCommand => {
                let Action::ProfileNotes(ref command) = self.action else {
                    unreachable!()
                };
                let (command_text, timeout) = command
                    .command()
                    .expect("profile note action was validated");
                return Some(Step {
                    command: command_text,
                    timeout: timeout.min(self.deadline.saturating_duration_since(now)),
                    write: command.mutating(),
                    sensitive: false,
                });
            }
            Stage::VoiceReadIMS | Stage::VoiceReadBackIMS => (r#"AT+QCFG="ims""#.into(), 5, false),
            Stage::VoiceReadVoLTE
            | Stage::VoiceReadBackVoLTE
            | Stage::VoiceVerifyVoLTEBeforeIMS => (r#"AT+QCFG="volte_disable""#.into(), 5, false),
            Stage::VoiceSetVoLTE => {
                let Action::CallModeVoice { target } = &self.action else {
                    unreachable!()
                };
                (
                    format!(r#"AT+QCFG="volte_disable",{}"#, target.volte_disabled),
                    8,
                    true,
                )
            }
            Stage::VoiceSetIMS => {
                let Action::CallModeVoice { target } = &self.action else {
                    unreachable!()
                };
                (format!(r#"AT+QCFG="ims",{}"#, target.ims), 8, true)
            }
            Stage::Done => return None,
        };
        Some(Step {
            command,
            timeout: Duration::from_secs(seconds).min(self.deadline.saturating_duration_since(now)),
            write,
            sensitive: false,
        })
    }
    fn success(&mut self) {
        self.outcome.complete = true;
        self.outcome.uncertain = false;
        self.outcome.error_code.clear();
        self.stage = Stage::Done;
    }
    pub(crate) fn accept(&mut self, mut reply: Reply) {
        let now = Instant::now();
        if matches!(self.stage, Stage::CallModeAdbAuthorizeSubmit) {
            // Some firmware echoes the authorization string. Keep only the
            // transport's terminal classification; never retain or serialize it.
            reply.text.zeroize();
        }
        let write = match self.stage {
            Stage::ProfileNotesCommand => self.action.mutating(),
            Stage::Set { .. }
            | Stage::Reboot
            | Stage::Diagnostic
            | Stage::CellularCallCommand
            | Stage::VoiceSetVoLTE
            | Stage::VoiceSetIMS
            | Stage::CallModeUsbCompositionWrite
            | Stage::CallModeAdbAuthorizeSubmit => true,
            _ => false,
        };
        if write {
            self.outcome.submitted = true;
            self.outcome.uncertain |= reply.uncertain;
        }
        if self.accept_restore_reply(&reply).is_err() {
            self.fail(restore::ERROR, self.outcome.submitted);
            return;
        }
        if matches!(self.stage, Stage::CallModeIdentityFirmware) {
            // IMEI is the binding authority; firmware is best effort. A broken
            // stream is still retired and never used for a follow-up write.
            self.retire |= reply.fatal || reply.uncertain;
            if reply.ok && !reply.fatal && !reply.uncertain {
                if let Some(identity) = self.outcome.module_identity.as_mut() {
                    identity.firmware = reply
                        .text
                        .lines()
                        .map(str::trim)
                        .filter(|line| {
                            !line.is_empty()
                                && !line.to_ascii_uppercase().starts_with("ATI")
                                && !line.eq_ignore_ascii_case("OK")
                        })
                        .collect::<Vec<_>>()
                        .join(" · ");
                }
            }
            self.success();
            return;
        }
        if reply.uncertain {
            // No terminal boundary means a later OK could belong to this
            // command. Never use another query to pretend stream alignment.
            self.retire = true;
            if is_call_mode_stage(self.stage) {
                self.outcome.response = if is_adb_stage(self.stage) || reply.text.is_empty() {
                    reply.code.clone()
                } else {
                    reply.text.clone()
                };
            }
            let code = call_mode_stage_failure_code(self.stage).unwrap_or(&reply.code);
            self.fail(code, write || self.unconfirmed_write());
            return;
        }
        if reply.fatal {
            self.retire = true;
            if is_call_mode_stage(self.stage) {
                self.outcome.response = if is_adb_stage(self.stage) || reply.text.is_empty() {
                    reply.code.clone()
                } else {
                    reply.text.clone()
                };
            }
            let code = call_mode_stage_failure_code(self.stage).unwrap_or(&reply.code);
            self.fail(code, self.unconfirmed_write());
            return;
        }
        if self.cancelled.load(Ordering::Acquire) {
            self.fail("radio_owner_revoked", self.unconfirmed_write());
            return;
        }
        if observed_call(&reply.text) && self.mutating() && !self.cellular_call() {
            self.fail("radio_call_busy", write);
            return;
        }
        match self.stage {
            Stage::CellularCallRead => {
                self.outcome.response = reply.text;
                if reply.ok && !contains_explicit_at_error_line(&self.outcome.response) {
                    self.outcome.verified = true;
                    self.success();
                } else {
                    self.fail("module_call_state_unconfirmed", false);
                }
            }
            Stage::CellularDialCalls => {
                if crate::module_radio::call_snapshot(reply.text).safe_idle {
                    self.stage = Stage::CellularCallCommand;
                } else {
                    self.fail("radio_call_busy", false);
                }
            }
            Stage::CellularCallCommand => {
                self.outcome.response = reply.text;
                if reply.ok && !contains_explicit_at_error_line(&self.outcome.response) {
                    self.outcome.verified = true;
                    self.success();
                } else {
                    self.fail("module_call_command_failed", false);
                }
            }
            Stage::Initial => {
                let Some(mode) = reply.ok.then(|| cfun(&reply.text)).flatten() else {
                    self.retire |= reply.uncertain;
                    self.fail("radio_read_unconfirmed", false);
                    return;
                };
                self.outcome.cfun = Some(mode);
                match self.action {
                    Action::Observe { cellular } => {
                        self.outcome.verified = true;
                        if cellular && mode != 4 {
                            self.stage = Stage::Cereg;
                        } else {
                            self.success();
                        }
                    }
                    Action::SetVerify { target } if target == mode => {
                        self.outcome.verified = true;
                        self.outcome.restore_confirmed = true;
                        self.success();
                    }
                    Action::SetVerify { target } => {
                        self.stage = Stage::Calls {
                            mode: target,
                            refresh_first: false,
                        }
                    }
                    Action::RefreshAfterProfileSwitch { .. } => {
                        self.stage = Stage::Calls {
                            mode: 0,
                            refresh_first: true,
                        }
                    }
                    Action::Reboot | Action::Diagnostic { .. } | Action::ProfileNotes(_) => {
                        unreachable!()
                    }
                    Action::CellularCallRead
                    | Action::CellularCall(_)
                    | Action::CallModeVoice { .. }
                    | Action::CallModeVoiceRead
                    | Action::CallModeIdentityRead
                    | Action::CallModeIdleRead
                    | Action::CallModeUsbCompositionRead
                    | Action::CallModeUsbCompositionWrite { .. }
                    | Action::CallModeAdbProbe
                    | Action::CallModeAdbAuthorize => unreachable!(),
                }
            }
            Stage::Calls {
                mode,
                refresh_first,
            } => {
                if !call_guard_allows(&reply) {
                    self.retire |= reply.uncertain;
                    self.fail("radio_call_state_unconfirmed", false);
                } else {
                    self.stage = Stage::Set {
                        mode,
                        refresh_first,
                    };
                }
            }
            Stage::Set {
                mode,
                refresh_first,
            } => {
                self.stage = Stage::Poll {
                    target: mode,
                    until: now + Duration::from_secs(if refresh_first { 8 } else { 10 }),
                    after: if refresh_first {
                        After::RefreshGap
                    } else {
                        After::Finish
                    },
                };
            }
            Stage::Poll {
                target,
                until,
                after,
            } => {
                if reply.ok
                    && let Some(mode) = cfun(&reply.text)
                {
                    self.outcome.cfun = Some(mode);
                    if mode == target {
                        self.outcome.uncertain = false;
                        match after {
                            After::Finish => {
                                self.outcome.verified = true;
                                self.outcome.restore_confirmed = true;
                                self.success();
                            }
                            After::RefreshGap => {
                                let Action::RefreshAfterProfileSwitch { target } = self.action
                                else {
                                    unreachable!()
                                };
                                self.stage = Stage::Calls {
                                    mode: target,
                                    refresh_first: false,
                                };
                                self.ready_at = now + Duration::from_secs(1);
                            }
                        }
                        return;
                    }
                }
                if now >= until {
                    self.retire |= reply.uncertain;
                    self.fail("radio_state_unverified", self.outcome.uncertain);
                } else {
                    self.ready_at = now + Duration::from_millis(250);
                }
            }
            Stage::Cereg | Stage::Creg | Stage::Signal if reply.uncertain => {
                self.retire = true;
                self.fail("radio_response_incomplete", false);
            }
            Stage::Cereg => {
                if reply.ok {
                    self.outcome.cereg = registration(&reply.text, "+CEREG:");
                }
                self.stage = Stage::Creg;
            }
            Stage::Creg => {
                if reply.ok {
                    self.outcome.creg = registration(&reply.text, "+CREG:");
                }
                self.stage = Stage::Signal;
            }
            Stage::Signal => {
                if reply.ok {
                    self.outcome.signal = signal(&reply.text);
                }
                self.success();
            }
            Stage::CallModeIdleCalls => {
                if reply.ok
                    && !contains_explicit_at_error_line(&reply.text)
                    && calls_idle(&reply.text)
                    && !reply
                        .text
                        .lines()
                        .any(|line| line.trim().starts_with("+CLCC:"))
                {
                    self.outcome.verified = true;
                    self.success();
                } else {
                    self.fail("radio_call_state_unconfirmed", false);
                }
            }
            Stage::CallModeIdentityIMEI => {
                let Some(imei) = (reply.ok && !contains_explicit_at_error_line(&reply.text))
                    .then(|| call_mode_imei(&reply.text))
                    .flatten()
                else {
                    self.fail("call_mode_identity_unconfirmed", false);
                    return;
                };
                self.outcome.module_identity = Some(CallModeModuleIdentity {
                    imei,
                    firmware: String::new(),
                });
                self.outcome.verified = true;
                self.stage = Stage::CallModeIdentityFirmware;
            }
            Stage::CallModeIdentityFirmware => unreachable!("best effort ATI handled above"),
            Stage::RebootCalls => {
                if call_guard_allows(&reply) {
                    self.stage = Stage::Reboot;
                } else {
                    self.retire |= reply.uncertain;
                    self.fail("radio_call_state_unconfirmed", false);
                }
            }
            Stage::Reboot => {
                if reply.ok && contains_explicit_at_error_line(&reply.text) {
                    self.outcome.response = reply.text;
                    self.fail("radio_reboot_rejected", false);
                } else if reply.ok {
                    self.success();
                } else {
                    self.retire |= reply.uncertain;
                    self.fail("radio_reboot_unconfirmed", reply.uncertain);
                }
            }
            Stage::DiagnosticCalls => {
                if call_guard_allows(&reply) {
                    self.stage = Stage::Diagnostic;
                } else {
                    self.retire |= reply.uncertain;
                    self.fail("radio_call_state_unconfirmed", false);
                }
            }
            Stage::ProfileNotesCalls => {
                if call_guard_allows(&reply) {
                    self.stage = Stage::ProfileNotesCommand;
                } else {
                    self.retire |= reply.uncertain;
                    self.fail("radio_call_state_unconfirmed", false);
                }
            }
            Stage::Diagnostic => {
                self.outcome.response = reply.text;
                if reply.ok {
                    self.success();
                } else {
                    self.retire |= reply.uncertain;
                    self.fail(&reply.code, reply.uncertain);
                }
            }
            Stage::ProfileNotesCommand => {
                self.outcome.response = reply.text;
                if reply.ok {
                    self.success();
                } else {
                    self.retire |= reply.uncertain;
                    self.fail(&reply.code, reply.uncertain);
                }
            }
            Stage::CallModeVoiceCalls => {
                if call_guard_allows(&reply) {
                    self.stage = Stage::VoiceReadIMS;
                } else {
                    self.outcome.response = reply.text;
                    self.fail("radio_call_state_unconfirmed", false);
                }
            }
            Stage::CallModeAdbProbe => {
                if reply.uncertain {
                    self.fail("call_mode_adb_probe_failed", false);
                    return;
                }
                let supported = reply.ok && crate::qadbkey::challenge(&reply.text).is_some();
                self.outcome.adb_authorization_supported = Some(supported);
                self.outcome.verified = true;
                self.outcome.response = if supported {
                    "旧式 ADB 挑战格式已确认".into()
                } else {
                    "模块未返回受支持的旧式 ADB 挑战".into()
                };
                self.success();
            }
            Stage::CallModeAdbAuthorizeCalls => {
                if call_guard_allows(&reply) {
                    self.stage = Stage::CallModeAdbAuthorizeChallenge;
                } else {
                    self.outcome.response = "通话状态无法确认；未提交 ADB 授权".into();
                    self.fail("radio_call_state_unconfirmed", false);
                }
            }
            Stage::CallModeAdbAuthorizeChallenge => {
                if !reply.ok {
                    self.outcome.response = "模块拒绝读取 ADB 授权挑战；未提交授权".into();
                    self.fail("call_mode_adb_challenge_rejected", false);
                    return;
                }
                let Some(challenge) = crate::qadbkey::challenge(&reply.text) else {
                    self.outcome.response =
                        "模块未返回唯一且有效的 8 位 ADB 授权挑战；未提交授权".into();
                    self.fail("call_mode_adb_challenge_invalid", false);
                    return;
                };
                self.adb_challenge = Some(challenge);
                self.stage = Stage::CallModeAdbAuthorizeSubmit;
            }
            Stage::CallModeAdbAuthorizeSubmit => {
                if reply.ok {
                    self.outcome.verified = true;
                    self.outcome.response = "模块已确认接受 ADB 持久授权".into();
                    self.adb_challenge = None;
                    self.success();
                } else {
                    self.outcome.response = "模块明确拒绝 ADB 持久授权；授权口令未记录".into();
                    self.adb_challenge = None;
                    self.fail("call_mode_adb_authorization_rejected", false);
                }
            }
            Stage::CallModeUsbCompositionRead => {
                if !reply.ok {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_usb_query_rejected", false);
                    return;
                }
                let Some(composition) = parse_usb_composition(&reply.text) else {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_usb_parse_failed", false);
                    return;
                };
                self.outcome.usb_composition = Some(composition);
                self.outcome.usb_composition_target = Some(composition.call_mode_target());
                self.outcome.verified = true;
                self.success();
            }
            Stage::CallModeUsbCompositionWriteCalls => {
                if call_guard_allows(&reply) {
                    self.stage = Stage::CallModeUsbCompositionReadBeforeWrite;
                } else {
                    self.outcome.response = reply.text;
                    self.fail("radio_call_state_unconfirmed", false);
                }
            }
            Stage::CallModeUsbCompositionReadBeforeWrite => {
                if !reply.ok {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_usb_query_rejected", false);
                    return;
                }
                let Some(actual) = parse_usb_composition(&reply.text) else {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_usb_parse_failed", false);
                    return;
                };
                let Action::CallModeUsbCompositionWrite { expected, target } = self.action else {
                    unreachable!()
                };
                if actual != expected {
                    self.outcome.usb_composition = Some(actual);
                    self.outcome.response = "当前 USB 配置与操作前快照不一致；未写入".into();
                    self.fail("call_mode_usb_precondition_changed", false);
                    return;
                }
                if actual == target {
                    self.outcome.usb_composition = Some(actual);
                    self.outcome.verified = true;
                    self.success();
                } else {
                    self.stage = Stage::CallModeUsbCompositionWrite;
                }
            }
            Stage::CallModeUsbCompositionWrite => {
                if !voice_write_accepted(&reply) {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_usb_write_rejected", false);
                    return;
                }
                self.stage = Stage::CallModeUsbCompositionReadBack;
            }
            Stage::CallModeUsbCompositionReadBack => {
                if !reply.ok {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_usb_readback_failed", true);
                    return;
                }
                let Some(actual) = parse_usb_composition(&reply.text) else {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_usb_readback_failed", true);
                    return;
                };
                let Action::CallModeUsbCompositionWrite { expected, target } = self.action else {
                    unreachable!()
                };
                if let Some(detail) = usb_readback_error(expected, target, actual) {
                    self.outcome.response = detail;
                    self.outcome.usb_composition = Some(actual);
                    self.fail("call_mode_usb_readback_unexpected", true);
                } else {
                    self.outcome.usb_composition = Some(actual);
                    self.outcome.verified = true;
                    self.success();
                }
            }
            Stage::VoiceReadIMS => {
                if !reply.ok {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_ims_read_failed", false);
                    return;
                }
                let Some((ims, volte_capability)) = parse_voice_ims(&reply.text) else {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_ims_read_failed", false);
                    return;
                };
                self.voice_current = Some(VoiceConfiguration {
                    ims,
                    volte_capability,
                    volte_disabled: 0,
                });
                self.stage = Stage::VoiceReadVoLTE;
            }
            Stage::VoiceReadVoLTE => {
                if !reply.ok {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_volte_read_failed", false);
                    return;
                }
                let Some(disabled) = parse_voice_volte_disabled(&reply.text) else {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_volte_read_failed", false);
                    return;
                };
                let Some(current) = self.voice_current.as_mut() else {
                    self.fail("call_mode_voice_state_invalid", false);
                    return;
                };
                current.volte_disabled = disabled;
                match self.action {
                    Action::CallModeVoice { target } => {
                        self.stage = if current.volte_disabled != target.volte_disabled {
                            Stage::VoiceSetVoLTE
                        } else if current.ims != target.ims {
                            Stage::VoiceSetIMS
                        } else {
                            Stage::VoiceReadBackIMS
                        };
                    }
                    Action::CallModeVoiceRead => {
                        self.outcome.voice = Some(*current);
                        self.outcome.verified = true;
                        self.success();
                    }
                    _ => unreachable!(),
                }
            }
            Stage::VoiceSetVoLTE => {
                if !voice_write_accepted(&reply) {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_volte_write_rejected", false);
                    return;
                }
                let Action::CallModeVoice { target } = self.action else {
                    unreachable!()
                };
                let current = self.voice_current.expect("validated initial voice state");
                self.stage = if self.restore_write.is_some() {
                    Stage::VoiceVerifyVoLTEBeforeIMS
                } else if current.ims != target.ims {
                    Stage::VoiceSetIMS
                } else {
                    Stage::VoiceReadBackIMS
                };
            }
            Stage::VoiceSetIMS => {
                if !voice_write_accepted(&reply) {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_ims_write_rejected", false);
                    return;
                }
                self.stage = Stage::VoiceReadBackIMS;
            }
            Stage::VoiceVerifyVoLTEBeforeIMS => {
                let Action::CallModeVoice { target } = self.action else {
                    unreachable!()
                };
                let current = self.voice_current.expect("validated initial voice state");
                if !reply.ok
                    || !parse_voice_volte_disabled(&reply.text).is_some_and(|actual| {
                        actual == current.volte_disabled || actual == target.volte_disabled
                    })
                {
                    self.fail("call_mode_volte_readback_failed", self.unconfirmed_write());
                    return;
                }
                self.stage = if current.ims != target.ims {
                    Stage::VoiceSetIMS
                } else {
                    Stage::VoiceReadBackIMS
                };
            }
            Stage::VoiceReadBackIMS => {
                if !reply.ok {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_ims_readback_failed", false);
                    return;
                }
                let Some((ims, volte_capability)) = parse_voice_ims(&reply.text) else {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_ims_readback_failed", false);
                    return;
                };
                let current = self.voice_current.expect("validated initial voice state");
                self.outcome.voice = Some(VoiceConfiguration {
                    ims,
                    volte_capability,
                    volte_disabled: current.volte_disabled,
                });
                self.stage = Stage::VoiceReadBackVoLTE;
            }
            Stage::VoiceReadBackVoLTE => {
                if !reply.ok {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_volte_readback_failed", false);
                    return;
                }
                let Some(disabled) = parse_voice_volte_disabled(&reply.text) else {
                    self.outcome.response = reply.text;
                    self.fail("call_mode_volte_readback_failed", false);
                    return;
                };
                let Some(actual) = self.outcome.voice.as_mut() else {
                    self.fail("call_mode_voice_state_invalid", false);
                    return;
                };
                actual.volte_disabled = disabled;
                let Action::CallModeVoice { target } = self.action else {
                    unreachable!()
                };
                let current = self.voice_current.expect("validated initial voice state");
                if let Some(detail) = voice_readback_error(current, target, *actual) {
                    self.outcome.response = detail;
                    self.fail("call_mode_voice_readback_unexpected", false);
                } else {
                    self.outcome.verified = true;
                    self.success();
                }
            }
            Stage::Done => {}
        }
    }
}
fn cfun(text: &str) -> Option<u8> {
    let mut values = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("+CFUN:"));
    let value = values.next()?.trim().parse::<u8>().ok()?;
    (value <= 4 && values.next().is_none()).then_some(value)
}
fn registration(text: &str, prefix: &str) -> Option<u8> {
    let line = text
        .lines()
        .find_map(|line| line.trim().strip_prefix(prefix))?;
    let fields: Vec<_> = line.split(',').map(str::trim).collect();
    let value = fields
        .get(if fields.len() > 1 { 1 } else { 0 })?
        .parse::<u8>()
        .ok()?;
    (value <= 10).then_some(value)
}
fn signal(text: &str) -> Option<u8> {
    let line = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("+CSQ:"))?;
    let value = line.split(',').next()?.trim().parse::<u8>().ok()?;
    (value <= 31 || value == 99).then_some(value)
}
fn is_call_mode_stage(stage: Stage) -> bool {
    matches!(
        stage,
        Stage::CallModeIdleCalls
            | Stage::CallModeIdentityIMEI
            | Stage::CallModeIdentityFirmware
            | Stage::CallModeVoiceCalls
            | Stage::CallModeUsbCompositionRead
            | Stage::CallModeUsbCompositionWriteCalls
            | Stage::CallModeUsbCompositionReadBeforeWrite
            | Stage::CallModeUsbCompositionWrite
            | Stage::CallModeUsbCompositionReadBack
            | Stage::VoiceReadIMS
            | Stage::VoiceReadVoLTE
            | Stage::VoiceSetVoLTE
            | Stage::VoiceSetIMS
            | Stage::VoiceReadBackIMS
            | Stage::VoiceReadBackVoLTE
            | Stage::VoiceVerifyVoLTEBeforeIMS
            | Stage::CallModeAdbProbe
            | Stage::CallModeAdbAuthorizeCalls
            | Stage::CallModeAdbAuthorizeChallenge
            | Stage::CallModeAdbAuthorizeSubmit
    )
}
fn is_adb_stage(stage: Stage) -> bool {
    matches!(
        stage,
        Stage::CallModeAdbProbe
            | Stage::CallModeAdbAuthorizeCalls
            | Stage::CallModeAdbAuthorizeChallenge
            | Stage::CallModeAdbAuthorizeSubmit
    )
}
fn call_mode_stage_failure_code(stage: Stage) -> Option<&'static str> {
    match stage {
        Stage::CallModeIdleCalls => Some("radio_call_state_unconfirmed"),
        Stage::CallModeIdentityIMEI => Some("call_mode_identity_unconfirmed"),
        Stage::CallModeIdentityFirmware => Some("call_mode_firmware_unconfirmed"),
        Stage::CallModeVoiceCalls => Some("radio_call_state_unconfirmed"),
        Stage::CallModeUsbCompositionRead => Some("call_mode_usb_read_failed"),
        Stage::CallModeUsbCompositionWriteCalls => Some("radio_call_state_unconfirmed"),
        Stage::CallModeUsbCompositionReadBeforeWrite => Some("call_mode_usb_read_failed"),
        Stage::CallModeUsbCompositionWrite => Some("call_mode_usb_write_unconfirmed"),
        Stage::CallModeUsbCompositionReadBack => Some("call_mode_usb_readback_failed"),
        Stage::VoiceReadIMS => Some("call_mode_ims_read_failed"),
        Stage::VoiceReadVoLTE => Some("call_mode_volte_read_failed"),
        Stage::VoiceSetVoLTE => Some("call_mode_volte_write_unconfirmed"),
        Stage::VoiceSetIMS => Some("call_mode_ims_write_unconfirmed"),
        Stage::VoiceReadBackIMS => Some("call_mode_ims_readback_failed"),
        Stage::VoiceReadBackVoLTE => Some("call_mode_volte_readback_failed"),
        Stage::CallModeAdbProbe => Some("call_mode_adb_probe_failed"),
        Stage::CallModeAdbAuthorizeCalls => Some("radio_call_state_unconfirmed"),
        Stage::CallModeAdbAuthorizeChallenge => Some("call_mode_adb_challenge_unconfirmed"),
        Stage::CallModeAdbAuthorizeSubmit => Some("call_mode_adb_authorization_unconfirmed"),
        _ => None,
    }
}
fn parse_voice_ims(text: &str) -> Option<(u8, u8)> {
    const MARKER: &str = "+qcfg: \"ims\",";
    for line in text.replace('\r', "\n").lines() {
        let line = line.trim();
        let lower = line.to_ascii_lowercase();
        let Some(index) = lower.find(MARKER) else {
            continue;
        };
        let values: Vec<_> = line[index + MARKER.len()..].split(',').collect();
        if values.len() < 2 {
            return None;
        }
        let ims = values[0].trim().parse::<u8>().ok()?;
        let capability = values[1].trim().parse::<u8>().ok()?;
        return (ims <= 2 && capability <= 1).then_some((ims, capability));
    }
    None
}
fn parse_voice_volte_disabled(text: &str) -> Option<u8> {
    const MARKER: &str = "+qcfg: \"volte/disable\",";
    for line in text.replace('\r', "\n").lines() {
        let line = line.trim();
        let lower = line.to_ascii_lowercase().replace('_', "/");
        let Some(index) = lower.find(MARKER) else {
            continue;
        };
        let value = line[index + MARKER.len()..].trim().parse::<u8>().ok()?;
        return (value <= 1).then_some(value);
    }
    None
}
fn parse_usb_composition(text: &str) -> Option<UsbComposition> {
    const MARKER: &str = "+qcfg: \"usbcfg\",";
    let mut parsed = None;
    for line in text.replace('\r', "\n").lines() {
        let line = line.trim();
        let lower = line.to_ascii_lowercase();
        let Some(index) = lower.find(MARKER) else {
            continue;
        };
        if parsed.is_some() {
            return None;
        }
        let fields: Vec<_> = line[index + MARKER.len()..]
            .split(',')
            .map(str::trim)
            .collect();
        if fields.len() != 9 {
            return None;
        }
        let vendor_id = parse_base_zero_i32(fields[0])?;
        let product_id = parse_base_zero_i32(fields[1])?;
        if !(1..=u16::MAX as i32).contains(&vendor_id)
            || !(1..=u16::MAX as i32).contains(&product_id)
        {
            return None;
        }
        let mut flags = [0; 7];
        for (index, field) in fields[2..].iter().enumerate() {
            let flag = parse_base_zero_i32(field)?;
            if !(0..=1).contains(&flag) {
                return None;
            }
            flags[index] = flag as u8;
        }
        parsed = Some(UsbComposition {
            vendor_id: vendor_id as u16,
            product_id: product_id as u16,
            flags,
        });
    }
    parsed.filter(|composition| composition.valid())
}
fn usb_composition_command(composition: UsbComposition) -> String {
    let mut command = format!(
        "AT+QCFG=\"USBCFG\",0x{:04X},0x{:04X}",
        composition.vendor_id, composition.product_id
    );
    for flag in composition.flags {
        command.push(',');
        command.push(char::from(b'0' + flag));
    }
    command
}
fn usb_readback_error(
    expected: UsbComposition,
    target: UsbComposition,
    actual: UsbComposition,
) -> Option<String> {
    if !expected.valid() || !target.valid() || !actual.valid() {
        return Some("USB 配置回读值无效".into());
    }
    if expected.vendor_id != target.vendor_id || expected.product_id != target.product_id {
        return Some("目标 USB VID/PID 与操作前配置不一致".into());
    }
    if actual.vendor_id != expected.vendor_id || actual.product_id != expected.product_id {
        return Some("USB 配置回读的 VID/PID 发生意外变化".into());
    }
    for index in 0..expected.flags.len() {
        let value = actual.flags[index];
        if expected.flags[index] == target.flags[index] {
            if value != expected.flags[index] {
                return Some(format!(
                    "USB 配置回读的第 {} 个未修改功能位发生变化",
                    index + 1
                ));
            }
        } else if value != expected.flags[index] && value != target.flags[index] {
            return Some(format!(
                "USB 配置回读的第 {} 个功能位不是原值或目标值",
                index + 1
            ));
        }
    }
    None
}
fn parse_base_zero_i32(raw: &str) -> Option<i32> {
    let raw = raw.trim();
    let (negative, unsigned) = match raw.as_bytes().first() {
        Some(b'-') => (true, &raw[1..]),
        Some(b'+') => (false, &raw[1..]),
        _ => (false, raw),
    };
    if unsigned.is_empty() {
        return None;
    }
    let (digits, radix) = if let Some(value) = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
    {
        (value, 16)
    } else if let Some(value) = unsigned
        .strip_prefix("0b")
        .or_else(|| unsigned.strip_prefix("0B"))
    {
        (value, 2)
    } else if let Some(value) = unsigned
        .strip_prefix("0o")
        .or_else(|| unsigned.strip_prefix("0O"))
    {
        (value, 8)
    } else if unsigned.starts_with('0') && unsigned.len() > 1 {
        (&unsigned[1..], 8)
    } else {
        (unsigned, 10)
    };
    let magnitude = i64::from_str_radix(digits, radix).ok()?;
    let signed = if negative { -magnitude } else { magnitude };
    i32::try_from(signed).ok()
}
fn voice_write_accepted(reply: &Reply) -> bool {
    reply.ok
        && !reply.text.lines().map(str::trim).any(|line| {
            line == "ERROR" || line.starts_with("+CME ERROR:") || line.starts_with("+CMS ERROR:")
        })
}

fn contains_explicit_at_error_line(response: &str) -> bool {
    response.lines().any(|line| {
        let line = line.trim().to_ascii_uppercase();
        line == "ERROR" || line.starts_with("+CME ERROR:") || line.starts_with("+CMS ERROR:")
    })
}

#[cfg(test)]
fn voice_readback_is_expected(
    current: VoiceConfiguration,
    target: VoiceConfiguration,
    actual: VoiceConfiguration,
) -> bool {
    voice_readback_error(current, target, actual).is_none()
}
pub(crate) fn voice_readback_error(
    current: VoiceConfiguration,
    target: VoiceConfiguration,
    actual: VoiceConfiguration,
) -> Option<String> {
    if !actual.valid() {
        return Some("IMS/VoLTE 回读值超出有效范围".into());
    }
    if actual.ims != current.ims && actual.ims != target.ims {
        return Some(format!(
            "IMS={} 既不是原值 {} 也不是目标值 {}",
            actual.ims, current.ims, target.ims
        ));
    }
    if actual.volte_disabled != current.volte_disabled
        && actual.volte_disabled != target.volte_disabled
    {
        return Some(format!(
            "volte_disabled={} 既不是原值 {} 也不是目标值 {}",
            actual.volte_disabled, current.volte_disabled, target.volte_disabled
        ));
    }
    if actual.volte_capability != current.volte_capability
        && actual.volte_capability != target.volte_capability
    {
        return Some(format!(
            "VoLTE capability={} 既不是原值 {} 也不是目标值 {}",
            actual.volte_capability, current.volte_capability, target.volte_capability
        ));
    }
    None
}
pub(crate) fn call_mode_imei(text: &str) -> Option<String> {
    // Do not truncate a longer decimal run into an apparently valid IMEI.
    text.split(|c: char| !c.is_ascii_digit())
        .find(|part| part.len() == 15)
        .map(str::to_owned)
}
pub(crate) fn observed_call(text: &str) -> bool {
    text.lines().map(str::trim).any(|line| {
        line == "RING"
            || line.starts_with("+CLIP:")
            || line == "CONNECT"
            || line.starts_with("CONNECT ")
    })
}
fn calls_idle(text: &str) -> bool {
    for line in text.lines().map(str::trim) {
        let Some(fields) = line.strip_prefix("+CLCC:") else {
            continue;
        };
        let fields: Vec<_> = fields.split(',').map(str::trim).collect();
        let Some(state) = fields.get(2).and_then(|value| value.parse::<u8>().ok()) else {
            return false;
        };
        if matches!(state, 2..=5)
            || state == 0
                && fields
                    .get(5)
                    .is_some_and(|number| !number.trim_matches('"').is_empty())
        {
            return false;
        }
        if state > 6 {
            return false;
        }
    }
    !observed_call(text)
}

fn call_guard_allows(reply: &Reply) -> bool {
    // CLCC is not implemented on every compatible modem. A complete explicit
    // rejection is not proof of a live call; policy reservations and native
    // serial URCs still fence the transaction. Partial/timeout cannot proceed.
    (reply.ok && calls_idle(&reply.text))
        || (!reply.ok && !reply.uncertain && !reply.fatal && reply.code == "radio_command_rejected")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsealed_call_mode_write_has_no_physical_commit_authority() {
        let expected = UsbComposition {
            vendor_id: 0x2c7c,
            product_id: 0x2400,
            flags: [1, 0, 1, 0, 1, 0, 0],
        };
        let mut plan = Plan::new(Action::CallModeUsbCompositionWrite {
            expected,
            target: expected.call_mode_target(),
        });
        let binding = Binding::Serial { serial_session: 1 };
        for response in ["OK", "+QCFG: \"usbcfg\",0x2C7C,0x2400,1,0,1,0,1,0,0\r\nOK"] {
            let read = plan.next().unwrap();
            assert!(!read.write);
            assert!(plan.validate_commit_before_write(&read, &binding).is_ok());
            assert!(plan.admission_guard_for_step(&read, binding.clone())());
            plan.accept(Reply::response(response.into()));
        }
        let write = plan.next().unwrap();
        assert!(write.write);
        assert_eq!(
            plan.validate_commit_before_write(&write, &binding),
            Err("call_mode_operation_lock_unavailable")
        );
        assert!(!plan.admission_guard_for_step(&write, binding)());
        let outcome = plan.finish();
        assert!(!outcome.submitted && !outcome.uncertain);
    }

    #[test]
    fn unrelated_rf_plan_does_not_require_a_call_mode_lock() {
        let mut plan = Plan::new(Action::SetVerify { target: 4 });
        let binding = Binding::Serial { serial_session: 1 };
        let read = plan.next().unwrap();
        assert!(!read.write);
        plan.accept(Reply::response("+CFUN: 1\r\nOK".into()));
        let calls = plan.next().unwrap();
        assert!(!calls.write);
        assert!(plan.validate_commit_before_write(&calls, &binding).is_ok());
        plan.accept(Reply::response("OK".into()));
        let write = plan.next().unwrap();
        assert!(write.write);
        assert!(plan.validate_commit_before_write(&write, &binding).is_ok());
        assert!(plan.admission_guard_for_step(&write, binding)());
    }
    fn step(plan: &mut Plan, command: &str, response: &str) {
        plan.ready_at = Instant::now();
        assert_eq!(plan.next().unwrap().command, command);
        plan.accept(Reply::response(response.into()));
    }
    #[test]
    fn shared_budget_expires_during_poll_gap_without_another_io() {
        let mut plan =
            Plan::new_with_budget(Action::SetVerify { target: 4 }, Duration::from_secs(55));
        step(&mut plan, "AT+CFUN?", "+CFUN: 1\r\nOK");
        step(&mut plan, "AT+CLCC", "OK");
        step(&mut plan, "AT+CFUN=4", "OK");
        plan.ready_at = Instant::now() + Duration::from_secs(1);
        plan.deadline = Instant::now() - Duration::from_millis(1);
        assert!(plan.next().is_none() && plan.done());
        let result = plan.finish();
        assert!(result.submitted && result.uncertain && !result.complete);
    }
    #[test]
    fn raw_classifier_covers_hayes_multiple_and_ambiguous_quotes() {
        for command in [
            "AT+CFUN=4",
            " ATe0 + Cfun = 1 ",
            "AT+CFUN=?;+CFUN=0",
            "AT+TEST=\"x\\\";+CFUN=1\"",
        ] {
            assert!(is_radio_write(command), "{command}");
        }
        for command in ["AT+CFUN?", "AT+CFUN=?", "AT+TEST=\"+CFUN=1\"", "AT+CSQ"] {
            assert!(!is_radio_write(command), "{command}");
        }
    }
    #[test]
    fn module_profile_note_commands_are_fixed_and_guarded_by_call_state() {
        let cases = [
            (ProfileNoteCommand::TestStorage, "AT+CPBS=?", false),
            (ProfileNoteCommand::SelectME, r#"AT+CPBS="ME""#, true),
            (ProfileNoteCommand::Status, "AT+CPBS?", false),
            (ProfileNoteCommand::ReadSupport, "AT+CPBR=?", false),
            (ProfileNoteCommand::WriteSupport, "AT+CPBW=?", false),
            (
                ProfileNoteCommand::Read { first: 1, last: 32 },
                "AT+CPBR=1,32",
                false,
            ),
            (
                ProfileNoteCommand::Write {
                    index: 7,
                    record: "VH1|8985|QQ|MQ|".into(),
                },
                r#"AT+CPBW=7,"00000000000",129,"VH1|8985|QQ|MQ|""#,
                true,
            ),
            (ProfileNoteCommand::Delete { index: 7 }, "AT+CPBW=7", true),
        ];
        for (command, expected, write) in cases {
            let mut plan = Plan::new(Action::ProfileNotes(command));
            let calls = plan.next().unwrap();
            assert_eq!(calls.command, "AT+CLCC");
            assert!(!calls.write);
            plan.accept(Reply::response("OK".into()));
            let action = plan.next().unwrap();
            assert_eq!(action.command, expected);
            assert_eq!(action.write, write);
            plan.accept(Reply::response("OK".into()));
            let result = plan.finish();
            assert!(result.complete);
            assert_eq!(result.submitted, write);
            assert!(!result.uncertain);
        }

        let invalid = Plan::new(Action::ProfileNotes(ProfileNoteCommand::Write {
            index: 1,
            record: "VH1|8985|x\";AT+CFUN=1|MQ|".into(),
        }));
        assert_eq!(
            invalid.action.validate().unwrap_err().code,
            "module_profile_notes_invalid_command"
        );
    }
    #[test]
    fn module_profile_note_write_does_not_run_during_call_or_replay_after_unknown_ack() {
        let action = Action::ProfileNotes(ProfileNoteCommand::Delete { index: 2 });
        let mut active_call = Plan::new(action.clone());
        assert_eq!(active_call.next().unwrap().command, "AT+CLCC");
        active_call.accept(Reply::response("+CLCC: 1,1,4,0,0,\"123\"\r\nOK".into()));
        assert!(active_call.done());
        assert!(active_call.next().is_none());
        let rejected = active_call.finish();
        assert!(!rejected.submitted && !rejected.complete);
        assert_eq!(rejected.error_code, "radio_call_state_unconfirmed");

        let mut unknown = Plan::new(action);
        assert_eq!(unknown.next().unwrap().command, "AT+CLCC");
        unknown.accept(Reply::response("OK".into()));
        assert_eq!(unknown.next().unwrap().command, "AT+CPBW=2");
        unknown.accept(Reply::response("+CPBW: 2".into()));
        assert!(unknown.done() && unknown.retire_transport());
        assert!(unknown.next().is_none());
        let result = unknown.finish();
        assert!(result.submitted && result.uncertain && !result.complete);
    }
    #[test]
    fn observe_confirms_any_mode_but_rf_off_never_probes_cellular() {
        for mode in 0..=4 {
            let mut plan = Plan::new(Action::Observe { cellular: false });
            step(&mut plan, "AT+CFUN?", &format!("+CFUN: {mode}\r\nOK"));
            let result = plan.finish();
            assert!(result.complete && result.verified && !result.submitted);
            assert_eq!(result.cfun, Some(mode));
        }
        let mut plan = Plan::new(Action::Observe { cellular: true });
        step(&mut plan, "AT+CFUN?", "+CFUN: 4\r\nOK");
        assert!(plan.done());
    }

    #[test]
    fn reboot_classifies_explicit_error_lines_in_an_otherwise_ok_response() {
        for response in [
            "ERROR\r\nOK",
            "+CME ERROR: 50\r\nOK",
            "AT+QCFG\r\n+CMS ERROR: 302\r\nOK",
        ] {
            let mut plan = Plan::new(Action::Reboot);
            step(&mut plan, "AT+CLCC", "OK");
            step(&mut plan, "AT+CFUN=1,1", response);
            let result = plan.finish();
            assert!(!result.complete, "accepted error response: {response:?}");
            assert!(
                !result.uncertain,
                "explicit rejection became uncertain: {response:?}"
            );
            assert_eq!(result.error_code, "radio_reboot_rejected");
            assert_eq!(result.response, response);
        }

        for response in ["OK", "+QCFG: \"error\",1\r\nOK"] {
            let mut plan = Plan::new(Action::Reboot);
            step(&mut plan, "AT+CLCC", "OK");
            step(&mut plan, "AT+CFUN=1,1", response);
            let result = plan.finish();
            assert!(
                result.complete,
                "valid reboot response rejected: {response:?}"
            );
        }
    }

    #[test]
    fn known_rejection_gets_one_readback_and_never_repeats_write() {
        let mut plan = Plan::new(Action::SetVerify { target: 4 });
        step(&mut plan, "AT+CFUN?", "+CFUN: 1\r\nOK");
        step(&mut plan, "AT+CLCC", "OK");
        step(&mut plan, "AT+CFUN=4", "ERROR");
        step(&mut plan, "AT+CFUN?", "+CFUN: 4\r\nOK");
        let result = plan.finish();
        assert!(result.complete && result.submitted && result.verified && !result.uncertain);
    }
    #[test]
    fn lost_write_ack_retires_without_readback_or_replay() {
        let mut plan = Plan::new(Action::SetVerify { target: 4 });
        step(&mut plan, "AT+CFUN?", "+CFUN: 1\r\nOK");
        step(&mut plan, "AT+CLCC", "OK");
        step(&mut plan, "AT+CFUN=4", "+CFUN: 4");
        assert!(plan.done() && plan.retire_transport());
        assert!(plan.next().is_none());
        let result = plan.finish();
        assert!(result.submitted && result.uncertain && !result.complete);
    }
    #[test]
    fn incoming_call_blocks_but_unsupported_clcc_does_not_disable_all_rf() {
        for (response, allowed) in [
            ("+CLCC: 1,1,4,0,0,\"123\"\r\nOK", false),
            ("ERROR", true),
            ("+CLCC: 1,0,6,0,0\r\nOK", true),
        ] {
            let mut plan = Plan::new(Action::SetVerify { target: 4 });
            step(&mut plan, "AT+CFUN?", "+CFUN: 1\r\nOK");
            step(&mut plan, "AT+CLCC", response);
            assert_eq!(plan.next().is_some(), allowed);
        }
    }
    #[test]
    fn network_recovery_reboot_rechecks_calls_before_the_physical_write() {
        // Even after the recovery worker's earlier USB/idle observations, the
        // actual reboot transport performs a fresh CLCC before CFUN=1,1.
        let mut plan = Plan::new(Action::Reboot);
        step(&mut plan, "AT+CLCC", "+CLCC: 1,1,4,0,0,\"123\"\r\nOK");
        assert!(plan.done());
        assert!(plan.next().is_none());
        let result = plan.finish();
        assert_eq!(result.error_code, "radio_call_state_unconfirmed");
        assert!(!result.submitted);
    }
    #[test]
    fn refresh_restores_latest_policy_target_in_one_plan() {
        let mut plan = Plan::new(Action::RefreshAfterProfileSwitch { target: 4 });
        for (command, response) in [
            ("AT+CFUN?", "+CFUN: 1\r\nOK"),
            ("AT+CLCC", "OK"),
            ("AT+CFUN=0", "OK"),
            ("AT+CFUN?", "+CFUN: 0\r\nOK"),
            ("AT+CLCC", "OK"),
            ("AT+CFUN=4", "OK"),
            ("AT+CFUN?", "+CFUN: 4\r\nOK"),
        ] {
            step(&mut plan, command, response);
        }
        let result = plan.finish();
        assert!(result.complete && result.restore_confirmed);
        assert_eq!(result.cfun, Some(4));
    }
    #[test]
    fn diagnostic_preserves_original_command_and_safe_debug() {
        let command = "ATE0;+CFUN=4";
        let mut plan = Plan::new(Action::Diagnostic {
            command: command.into(),
            timeout_ms: 20_000,
        });
        step(&mut plan, "AT+CLCC", "OK");
        step(&mut plan, command, "private device response\r\nOK");
        let result = plan.finish();
        assert!(result.complete && result.submitted && !result.verified);
        assert!(result.response.contains("private device response"));
        assert!(!format!("{result:?}").contains("private device response"));
    }
    #[test]
    fn call_mode_voice_read_is_two_read_only_queries() {
        let mut plan = Plan::new(Action::CallModeVoiceRead);
        assert!(!plan.mutating());
        step(&mut plan, r#"AT+QCFG="ims""#, "+QCFG: \"ims\",2,0\r\nOK");
        step(
            &mut plan,
            r#"AT+QCFG="volte_disable""#,
            "+QCFG: \"volte/disable\",1\r\nOK",
        );
        let result = plan.finish();
        assert!(result.complete && result.verified && !result.submitted);
        assert_eq!(
            result.voice,
            Some(VoiceConfiguration {
                ims: 2,
                volte_capability: 0,
                volte_disabled: 1,
            })
        );
    }
    #[test]
    fn call_mode_usb_composition_read_is_read_only_and_returns_validated_fields() {
        let mut plan = Plan::new(Action::CallModeUsbCompositionRead);
        assert!(!plan.mutating());
        step(
            &mut plan,
            r#"AT+QCFG="USBCFG""#,
            "AT+QCFG=\"USBCFG\"\r\n+QCFG: \"usbcfg\",0x2CA3,0x4006,1,1,1,1,1,0,1\r\nOK",
        );
        let result = plan.finish();
        assert!(result.complete && result.verified && !result.submitted);
        assert_eq!(
            result.usb_composition,
            Some(UsbComposition {
                vendor_id: 0x2ca3,
                product_id: 0x4006,
                flags: [1, 1, 1, 1, 1, 0, 1],
            })
        );
        assert_eq!(
            result.usb_composition_target,
            Some(UsbComposition {
                vendor_id: 0x2ca3,
                product_id: 0x4006,
                flags: [1, 1, 1, 1, 1, 1, 1],
            })
        );
    }
    #[test]
    fn call_mode_usb_composition_read_rejects_ambiguous_or_malformed_output() {
        for response in [
            "+QCFG: \"USBCFG\",0x2CA3,0x4006,1,1,1,1,1,0\r\nOK",
            "+QCFG: \"USBCFG\",0x2CA3,0x4006,1,1,1,1,1,0,1,9\r\nOK",
            "+QCFG: \"USBCFG\",0x2CA3,0x4006,1,1,1,1,1,2,1\r\nOK",
            "+QCFG: \"USBCFG\",0,0x4006,1,1,1,1,1,0,1\r\nOK",
            "+QCFG: \"USBCFG\",0x2CA3,0x4006,1,1,1,1,1,0,1\r\n+QCFG: \"USBCFG\",0x2CA3,0x4006,1,1,1,1,1,0,1\r\nOK",
        ] {
            let mut plan = Plan::new(Action::CallModeUsbCompositionRead);
            step(&mut plan, r#"AT+QCFG="USBCFG""#, response);
            let result = plan.finish();
            assert!(!result.complete && !result.verified && !result.submitted);
            assert_eq!(result.error_code, "call_mode_usb_parse_failed");
        }
        let mut rejected = Plan::new(Action::CallModeUsbCompositionRead);
        step(&mut rejected, r#"AT+QCFG="USBCFG""#, "ERROR");
        assert_eq!(rejected.finish().error_code, "call_mode_usb_query_rejected");
    }

    #[test]
    fn call_mode_adb_probe_returns_only_a_supported_flag() {
        let mut plan = Plan::new(Action::CallModeAdbProbe);
        step(
            &mut plan,
            "AT+QADBKEY?",
            "AT+QADBKEY?\r\n+QADBKEY: 12345678\r\nOK",
        );
        let result = plan.finish();
        assert!(result.complete && result.verified && !result.submitted);
        assert_eq!(result.adb_authorization_supported, Some(true));
        assert!(!result.response.contains("12345678"));

        let mut unsupported = Plan::new(Action::CallModeAdbProbe);
        step(&mut unsupported, "AT+QADBKEY?", "ERROR");
        let result = unsupported.finish();
        assert!(result.complete && result.verified);
        assert_eq!(result.adb_authorization_supported, Some(false));
    }

    #[test]
    fn call_mode_adb_authorization_derives_and_scrubs_echoed_password() {
        let password = "0jXKXQwSwMxYoeg";
        let mut plan = Plan::new(Action::CallModeAdbAuthorize);
        step(&mut plan, "AT+CLCC", "OK");
        step(
            &mut plan,
            "AT+QADBKEY?",
            "AT+QADBKEY?\r\n+QADBKEY: 12345678\r\nOK",
        );
        let mut submission = plan.next().expect("authorization command");
        assert!(submission.write && submission.sensitive);
        assert_eq!(submission.command, format!("AT+QADBKEY=\"{password}\""));
        plan.mark_submitted();
        plan.accept(Reply::response(format!("{}\r\nOK", submission.command)));
        submission.command.zeroize();

        let result = plan.finish();
        assert!(result.complete && result.submitted && result.verified);
        assert!(!result.response.contains(password));
        assert_eq!(result.response, "模块已确认接受 ADB 持久授权");
    }

    #[test]
    fn call_mode_adb_authorization_rejection_never_retains_echoed_password() {
        let password = "0jXKXQwSwMxYoeg";
        let mut plan = Plan::new(Action::CallModeAdbAuthorize);
        step(&mut plan, "AT+CLCC", "OK");
        step(&mut plan, "AT+QADBKEY?", "+QADBKEY: 12345678\r\nOK");
        let mut submission = plan.next().unwrap();
        plan.mark_submitted();
        plan.accept(Reply::response(format!("{}\r\nERROR", submission.command)));
        submission.command.zeroize();
        let result = plan.finish();
        assert!(!result.complete && result.submitted && !result.uncertain);
        assert_eq!(result.error_code, "call_mode_adb_authorization_rejected");
        assert!(!result.response.contains(password));
    }

    #[test]
    fn call_mode_adb_authorization_uncertainty_retires_without_replay_or_secret_response() {
        let password = "0jXKXQwSwMxYoeg";
        let mut plan = Plan::new(Action::CallModeAdbAuthorize);
        step(&mut plan, "AT+CLCC", "OK");
        step(&mut plan, "AT+QADBKEY?", "+QADBKEY: 12345678\r\nOK");
        let mut submission = plan.next().unwrap();
        let echoed = format!("{}\r\n", submission.command);
        plan.mark_submitted();
        plan.accept(Reply {
            text: echoed,
            ok: false,
            uncertain: true,
            fatal: false,
            code: "serial_timeout".into(),
        });
        submission.command.zeroize();

        assert!(plan.done() && plan.retire_transport());
        assert!(plan.next().is_none());
        let result = plan.finish();
        assert!(result.submitted && result.uncertain && !result.complete);
        assert_eq!(result.error_code, "call_mode_adb_authorization_unconfirmed");
        assert_eq!(result.response, "serial_timeout");
        assert!(!result.response.contains(password));
    }

    #[test]
    fn call_mode_usb_composition_write_checks_snapshot_writes_once_and_verifies_readback() {
        let expected = UsbComposition {
            vendor_id: 0x2ca3,
            product_id: 0x4006,
            flags: [1, 1, 1, 1, 1, 0, 0],
        };
        let target = UsbComposition {
            flags: [1, 1, 1, 1, 1, 1, 1],
            ..expected
        };
        let mut plan = Plan::new(Action::CallModeUsbCompositionWrite { expected, target });
        step(&mut plan, "AT+CLCC", "OK");
        step(
            &mut plan,
            r#"AT+QCFG="USBCFG""#,
            "+QCFG: \"USBCFG\",0x2CA3,0x4006,1,1,1,1,1,0,0\r\nOK",
        );
        step(
            &mut plan,
            r#"AT+QCFG="USBCFG",0x2CA3,0x4006,1,1,1,1,1,1,1"#,
            "OK",
        );
        step(
            &mut plan,
            r#"AT+QCFG="USBCFG""#,
            "+QCFG: \"USBCFG\",0x2CA3,0x4006,1,1,1,1,1,1,1\r\nOK",
        );
        let result = plan.finish();
        assert!(result.complete && result.verified && result.submitted && !result.uncertain);
        assert_eq!(result.usb_composition, Some(target));
    }
    #[test]
    fn call_mode_usb_composition_write_fails_closed_on_changed_snapshot_and_bad_readback() {
        let expected = UsbComposition {
            vendor_id: 0x2ca3,
            product_id: 0x4006,
            flags: [1, 1, 1, 1, 1, 0, 0],
        };
        let target = UsbComposition {
            flags: [1, 1, 1, 1, 1, 1, 1],
            ..expected
        };
        let mut changed = Plan::new(Action::CallModeUsbCompositionWrite { expected, target });
        step(&mut changed, "AT+CLCC", "OK");
        step(
            &mut changed,
            r#"AT+QCFG="USBCFG""#,
            "+QCFG: \"USBCFG\",0x2CA3,0x4006,1,1,1,1,1,1,0\r\nOK",
        );
        assert_eq!(
            changed.finish().error_code,
            "call_mode_usb_precondition_changed"
        );

        let mut unexpected = Plan::new(Action::CallModeUsbCompositionWrite { expected, target });
        step(&mut unexpected, "AT+CLCC", "OK");
        step(
            &mut unexpected,
            r#"AT+QCFG="USBCFG""#,
            "+QCFG: \"USBCFG\",0x2CA3,0x4006,1,1,1,1,1,0,0\r\nOK",
        );
        step(
            &mut unexpected,
            r#"AT+QCFG="USBCFG",0x2CA3,0x4006,1,1,1,1,1,1,1"#,
            "OK",
        );
        step(
            &mut unexpected,
            r#"AT+QCFG="USBCFG""#,
            "+QCFG: \"USBCFG\",0x2CA3,0x4006,1,1,1,1,0,1,1\r\nOK",
        );
        let result = unexpected.finish();
        assert!(!result.complete && result.submitted && result.uncertain);
        assert_eq!(result.error_code, "call_mode_usb_readback_unexpected");
    }
    #[test]
    fn call_mode_usb_composition_write_rejection_never_replays_or_reads_back() {
        let expected = UsbComposition {
            vendor_id: 0x2ca3,
            product_id: 0x4006,
            flags: [1, 1, 1, 1, 1, 0, 0],
        };
        let target = UsbComposition {
            flags: [1, 1, 1, 1, 1, 1, 1],
            ..expected
        };
        let mut plan = Plan::new(Action::CallModeUsbCompositionWrite { expected, target });
        step(&mut plan, "AT+CLCC", "OK");
        step(
            &mut plan,
            r#"AT+QCFG="USBCFG""#,
            "+QCFG: \"USBCFG\",0x2CA3,0x4006,1,1,1,1,1,0,0\r\nOK",
        );
        step(
            &mut plan,
            r#"AT+QCFG="USBCFG",0x2CA3,0x4006,1,1,1,1,1,1,1"#,
            "ERROR",
        );
        let result = plan.finish();
        assert!(!result.complete && result.submitted && !result.uncertain);
        assert_eq!(result.error_code, "call_mode_usb_write_rejected");
    }
    #[test]
    fn partial_optional_registration_stops_before_next_command() {
        let mut plan = Plan::new(Action::Observe { cellular: true });
        step(&mut plan, "AT+CFUN?", "+CFUN: 1\r\nOK");
        step(&mut plan, "AT+CEREG?", "+CEREG: 2,1");
        assert!(plan.done() && plan.retire_transport());
        assert!(!plan.finish().complete);
    }
    #[test]
    fn call_mode_voice_writes_in_order_and_accepts_reboot_effective_readback() {
        let target = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        let mut plan = Plan::new(Action::CallModeVoice { target });
        for (command, response) in [
            ("AT+CLCC", "OK"),
            (r#"AT+QCFG="ims""#, "+QCFG: \"ims\",2,0\r\nOK"),
            (
                r#"AT+QCFG="volte_disable""#,
                "+QCFG: \"volte/disable\",1\r\nOK",
            ),
            (r#"AT+QCFG="volte_disable",0"#, "OK"),
            (r#"AT+QCFG="ims",1"#, "OK"),
            (r#"AT+QCFG="ims""#, "+QCFG: \"ims\",2,0\r\nOK"),
            (
                r#"AT+QCFG="volte_disable""#,
                "+QCFG: \"volte_disable\",0\r\nOK",
            ),
        ] {
            step(&mut plan, command, response);
        }
        let result = plan.finish();
        assert!(result.complete && result.submitted && result.verified);
        assert_eq!(
            result.voice,
            Some(VoiceConfiguration {
                ims: 2,
                volte_capability: 0,
                volte_disabled: 0,
            })
        );
    }
    #[test]
    fn call_mode_voice_unchanged_target_still_reads_back_without_writing() {
        let target = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        let mut plan = Plan::new(Action::CallModeVoice { target });
        for (command, response) in [
            ("AT+CLCC", "OK"),
            (r#"AT+QCFG="ims""#, "+QCFG: \"ims\",1,1\r\nOK"),
            (
                r#"AT+QCFG="volte_disable""#,
                "+QCFG: \"volte_disable\",0\r\nOK",
            ),
            (r#"AT+QCFG="ims""#, "+QCFG: \"ims\",1,1\r\nOK"),
            (
                r#"AT+QCFG="volte_disable""#,
                "+QCFG: \"volte/disable\",0\r\nOK",
            ),
        ] {
            step(&mut plan, command, response);
        }
        let result = plan.finish();
        assert!(result.complete && result.verified && !result.submitted);
        assert_eq!(result.voice, Some(target));
    }
    #[test]
    fn call_mode_voice_rejects_unexpected_readback_and_does_not_replay_rejected_write() {
        let target = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        let mut rejected = Plan::new(Action::CallModeVoice { target });
        step(&mut rejected, "AT+CLCC", "OK");
        step(
            &mut rejected,
            r#"AT+QCFG="ims""#,
            "+QCFG: \"ims\",2,0\r\nOK",
        );
        step(
            &mut rejected,
            r#"AT+QCFG="volte_disable""#,
            "+QCFG: \"volte_disable\",1\r\nOK",
        );
        step(&mut rejected, r#"AT+QCFG="volte_disable",0"#, "ERROR");
        assert!(rejected.done() && !rejected.retire_transport());
        let result = rejected.finish();
        assert!(result.submitted && !result.complete && !result.uncertain);
        assert_eq!(result.error_code, "call_mode_volte_write_rejected");

        let mut unexpected = Plan::new(Action::CallModeVoice { target });
        for (command, response) in [
            ("AT+CLCC", "OK"),
            (r#"AT+QCFG="ims""#, "+QCFG: \"ims\",2,0\r\nOK"),
            (
                r#"AT+QCFG="volte_disable""#,
                "+QCFG: \"volte_disable\",1\r\nOK",
            ),
            (r#"AT+QCFG="volte_disable",0"#, "OK"),
            (r#"AT+QCFG="ims",1"#, "OK"),
            (r#"AT+QCFG="ims""#, "+QCFG: \"ims\",0,0\r\nOK"),
            (
                r#"AT+QCFG="volte_disable""#,
                "+QCFG: \"volte_disable\",0\r\nOK",
            ),
        ] {
            step(&mut unexpected, command, response);
        }
        let result = unexpected.finish();
        assert!(!result.complete && !result.verified);
        assert_eq!(result.error_code, "call_mode_voice_readback_unexpected");
    }
    #[test]
    fn call_mode_voice_rejects_embedded_error_and_active_call_before_configuration_writes() {
        let target = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        let mut embedded_error = Plan::new(Action::CallModeVoice { target });
        step(&mut embedded_error, "AT+CLCC", "OK");
        step(
            &mut embedded_error,
            r#"AT+QCFG="ims""#,
            "+QCFG: \"ims\",2,0\r\nOK",
        );
        step(
            &mut embedded_error,
            r#"AT+QCFG="volte_disable""#,
            "+QCFG: \"volte_disable\",1\r\nOK",
        );
        step(
            &mut embedded_error,
            r#"AT+QCFG="volte_disable",0"#,
            "ERROR\r\nOK",
        );
        let result = embedded_error.finish();
        assert!(result.submitted && !result.complete && !result.uncertain);
        assert_eq!(result.error_code, "call_mode_volte_write_rejected");

        let mut active_call = Plan::new(Action::CallModeVoice { target });
        step(
            &mut active_call,
            "AT+CLCC",
            "+CLCC: 1,1,4,0,0,\"123\"\r\nOK",
        );
        assert!(active_call.done());
        assert!(active_call.next().is_none());
        let result = active_call.finish();
        assert!(!result.submitted && !result.complete);
        assert_eq!(result.error_code, "radio_call_state_unconfirmed");
    }
    #[test]
    fn call_mode_voice_lost_write_ack_retires_the_transport_without_readback() {
        let target = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        let mut plan = Plan::new(Action::CallModeVoice { target });
        step(&mut plan, "AT+CLCC", "OK");
        step(&mut plan, r#"AT+QCFG="ims""#, "+QCFG: \"ims\",2,1\r\nOK");
        step(
            &mut plan,
            r#"AT+QCFG="volte_disable""#,
            "+QCFG: \"volte_disable\",1\r\nOK",
        );
        step(&mut plan, r#"AT+QCFG="volte_disable",0"#, "OK");
        step(&mut plan, r#"AT+QCFG="ims",1"#, "+QCFG: 1");
        assert!(plan.done() && plan.retire_transport());
        assert!(plan.next().is_none());
        let result = plan.finish();
        assert!(result.submitted && result.uncertain && !result.complete);
        assert_eq!(result.error_code, "call_mode_ims_write_unconfirmed");
    }
    #[test]
    fn voice_readback_compares_each_field_to_only_its_old_or_target_value() {
        let current = VoiceConfiguration {
            ims: 2,
            volte_capability: 0,
            volte_disabled: 1,
        };
        let target = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        for actual in [
            current,
            target,
            VoiceConfiguration {
                ims: 1,
                volte_capability: 0,
                volte_disabled: 1,
            },
            VoiceConfiguration {
                ims: 2,
                volte_capability: 1,
                volte_disabled: 0,
            },
        ] {
            assert!(voice_readback_is_expected(current, target, actual));
        }
        for actual in [
            VoiceConfiguration { ims: 0, ..current },
            VoiceConfiguration {
                volte_capability: 2,
                ..current
            },
            VoiceConfiguration {
                volte_disabled: 2,
                ..current
            },
        ] {
            assert!(!voice_readback_is_expected(current, target, actual));
        }
        assert!(
            Action::CallModeVoice {
                target: VoiceConfiguration { ims: 3, ..target }
            }
            .validate()
            .is_err()
        );
    }
}

#[cfg(test)]
mod call_mode_identity_tests {
    use super::*;
    #[test]
    fn strict_identity_proof_has_no_write_and_ati_is_best_effort() {
        for firmware in ["ATI\r\nQuectel\r\nRevision: fixture\r\nOK\r\n", "ERROR\r\n"] {
            let mut plan = Plan::new(Action::CallModeIdentityRead);
            let step = plan.next().unwrap();
            assert_eq!(step.command, "AT+CGSN");
            assert!(!step.write);
            plan.accept(Reply::response(
                "AT+CGSN\r\n+CGSN: \"123456789012345\"\r\nOK\r\n".into(),
            ));
            let step = plan.next().unwrap();
            assert_eq!(step.command, "ATI");
            assert!(!step.write);
            plan.accept(Reply::response(firmware.into()));
            let outcome = plan.finish();
            assert!(outcome.complete && outcome.verified && !outcome.submitted);
            let identity = outcome.module_identity.unwrap();
            assert_eq!(identity.imei, "123456789012345");
            if firmware.starts_with("ERROR") {
                assert!(identity.firmware.is_empty());
            } else {
                assert_eq!(identity.firmware, "Quectel · Revision: fixture");
            }
        }
    }
    #[test]
    fn malformed_or_rejected_identity_never_runs_ati() {
        for response in [
            "1234567890123456\r\nOK\r\n",
            "１２３４５６７８９０１２３４５\r\nOK\r\n",
            "123456789012345\r\nERROR\r\n",
            "123456789012345\r\nERROR\r\nOK\r\n",
        ] {
            let mut plan = Plan::new(Action::CallModeIdentityRead);
            plan.next().unwrap();
            plan.accept(Reply::response(response.into()));
            assert!(plan.done());
            assert!(plan.next().is_none());
            let outcome = plan.finish();
            assert!(!outcome.complete && !outcome.submitted);
            assert_eq!(outcome.error_code, "call_mode_identity_unconfirmed");
        }
    }
    #[test]
    fn advisory_firmware_timeout_retires_stream_without_losing_imei() {
        let mut plan = Plan::new(Action::CallModeIdentityRead);
        plan.next().unwrap();
        plan.accept(Reply::response("123456789012345\r\nOK\r\n".into()));
        plan.next().unwrap();
        plan.accept(Reply {
            text: String::new(),
            ok: false,
            uncertain: true,
            fatal: false,
            code: "usb_at_timeout".into(),
        });
        assert!(plan.retire_transport());
        let outcome = plan.finish();
        assert!(outcome.verified && outcome.complete && !outcome.uncertain);
        assert_eq!(outcome.module_identity.unwrap().imei, "123456789012345");
    }
    #[test]
    fn fixed_idle_admission_requires_fresh_successful_empty_clcc() {
        for response in [
            "OK\r\n",
            "ERROR\r\n",
            "+CLCC: 1,0,1,0,0\r\nOK\r\n",
            "+CLCC: 1,0,0,0,0,\"\",129\r\nOK\r\n",
            "RING\r\nOK\r\n",
        ] {
            let mut plan = Plan::new(Action::CallModeIdleRead);
            let step = plan.next().unwrap();
            assert_eq!(step.command, "AT+CLCC");
            assert!(!step.write);
            plan.accept(Reply::response(response.into()));
            let outcome = plan.finish();
            assert_eq!(outcome.complete, response == "OK\r\n");
            assert_eq!(outcome.verified, response == "OK\r\n");
            assert!(!outcome.submitted);
        }
    }
}

#[derive(Clone)]
pub(crate) struct CallCommand {
    pub(crate) action: crate::module_radio::CallAction,
    pub(crate) value: String,
    pub(crate) admission: crate::module_radio::CallAdmission,
}
impl std::fmt::Debug for CallCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallCommand")
            .field("action", &self.action)
            .finish_non_exhaustive()
    }
}
impl CallCommand {
    fn command(&self) -> Result<(String, Duration), Failure> {
        use crate::module_radio::CallAction;
        let value = self.value.as_bytes();
        match self.action {
            CallAction::Dial
                if !value.is_empty()
                    && value.len() <= 32
                    && value != b"+"
                    && value.iter().enumerate().all(|(i, b)| {
                        b.is_ascii_digit() || matches!(b, b'*' | b'#') || (*b == b'+' && i == 0)
                    }) =>
            {
                Ok((format!("ATD{};", self.value), Duration::from_secs(10)))
            }
            CallAction::Answer if value.is_empty() => Ok(("ATA".into(), Duration::from_secs(10))),
            CallAction::Hangup if value.is_empty() => Ok(("ATH".into(), Duration::from_secs(10))),
            CallAction::Dtmf
                if value.len() == 1
                    && (value[0].is_ascii_digit() || matches!(value[0], b'*' | b'#')) =>
            {
                Ok((format!("AT+VTS={}", self.value), Duration::from_secs(5)))
            }
            _ => Err(failure("module_call_invalid_command")),
        }
    }
}

pub(crate) fn is_cellular_call_write(command: &str) -> bool {
    [true, false].into_iter().any(|escapes| {
        let text = unquoted_command(command, escapes);
        text.split(';').enumerate().any(|(index, part)| {
            if part.contains("+VTS=") {
                return true;
            }
            let body = if let Some(body) = part.strip_prefix("AT") {
                body
            } else if index != 0 {
                part
            } else {
                return false;
            };
            !body.starts_with(['+', '^', '$'])
                && body.bytes().any(|byte| matches!(byte, b'D' | b'A' | b'H'))
        })
    })
}

#[cfg(test)]
mod cellular_call_tests {
    use super::*;
    use crate::module_radio::CallAction;
    fn plan(action: CallAction, value: &str) -> Plan {
        Plan::new(Action::CellularCall(CallCommand {
            action,
            value: value.into(),
            admission: Arc::new(|| true),
        }))
    }
    #[test]
    fn dial_proves_idle_in_same_plan_and_does_not_replay_uncertain_submission() {
        let mut plan = plan(CallAction::Dial, "+123");
        assert_eq!(plan.next().unwrap().command, "AT+CLCC");
        plan.accept(Reply::response("OK".into()));
        assert_eq!(plan.next().unwrap().command, "ATD+123;");
        plan.mark_submitted();
        plan.accept(Reply {
            text: String::new(),
            ok: false,
            uncertain: true,
            fatal: false,
            code: "serial_timeout".into(),
        });
        assert!(plan.done());
        assert!(plan.next().is_none());
        assert!(plan.retire_transport());
        let outcome = plan.finish();
        assert!(outcome.submitted && outcome.uncertain && !outcome.complete);
    }
    #[test]
    fn held_unknown_and_numberless_calls_block_dial_before_write() {
        for response in [
            "+CLCC: 1,0,1,0,0,\"123\"\r\nOK",
            "+CLCC: invalid\r\nOK",
            "+CLCC: 1,0,0,0,0\r\nOK",
        ] {
            let mut plan = plan(CallAction::Dial, "123");
            plan.next().unwrap();
            plan.accept(Reply::response(response.into()));
            assert!(plan.done());
            assert!(plan.next().is_none());
            assert!(!plan.finish().submitted);
        }
    }
    #[test]
    fn queued_admission_change_prevents_actual_dial_and_cancel_prevents_ata() {
        let admitted = Arc::new(AtomicBool::new(true));
        let captured = admitted.clone();
        let mut plan = Plan::new(Action::CellularCall(CallCommand {
            action: CallAction::Dial,
            value: "123".into(),
            admission: Arc::new(move || captured.load(Ordering::Acquire)),
        }));
        plan.next().unwrap();
        plan.accept(Reply::response("OK".into()));
        admitted.store(false, Ordering::Release);
        assert!(plan.next().is_none());
        assert!(!plan.finish().submitted);
        let cancelled = Arc::new(AtomicBool::new(true));
        let mut plan = Plan::new_with_cancelled(
            Action::CellularCall(CallCommand {
                action: CallAction::Answer,
                value: String::new(),
                admission: Arc::new(|| true),
            }),
            Duration::from_secs(10),
            cancelled,
        );
        assert!(plan.next().is_none());
        assert!(!plan.finish().submitted);
    }
    #[test]
    fn fixed_answer_hangup_dtmf_allow_active_call_urcs() {
        for (action, value, expected) in [
            (CallAction::Answer, "", "ATA"),
            (CallAction::Hangup, "", "ATH"),
            (CallAction::Dtmf, "#", "AT+VTS=#"),
        ] {
            let mut plan = plan(action, value);
            assert_eq!(plan.next().unwrap().command, expected);
            plan.accept(Reply::response("CONNECT\r\nOK".into()));
            let outcome = plan.finish();
            assert!(outcome.complete && outcome.verified && outcome.submitted);
        }
    }
    #[test]
    fn fixed_call_validation_rejects_at_injection_and_raw_call_detector_ignores_quoted_payload() {
        for value in ["+", "123;ATH", "123\r\nATH", "", "hello"] {
            assert!(
                Action::CellularCall(CallCommand {
                    action: CallAction::Dial,
                    value: value.into(),
                    admission: Arc::new(|| true)
                })
                .validate()
                .is_err()
            );
        }
        for text in [
            "ATD123;",
            "ATA",
            "ATH",
            "AT+VTS=1",
            "AT+CSQ;ATH",
            "AT+CSQ;H",
            "AT+CSQ;A",
            "AT+CSQ;D123;",
            "ATE0D123;",
        ] {
            assert!(is_cellular_call_write(text), "{text}");
        }
        assert!(!is_cellular_call_write("AT+CPBW=1,\"123\",129,\"ATH\""));
    }
}
