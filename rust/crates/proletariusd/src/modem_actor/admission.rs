//! Admission is decided by the physical owner, after dequeue and before a new
//! finite operation. Existing leases and plans retain their exact settlement
//! identity; neither an old serial session nor a module hold grants new work.
use super::*;

fn revoked() -> ActorError {
    ActorError::new(
        crate::module_recovery::CODE,
        crate::module_recovery::MESSAGE,
    )
}

impl ActorState {
    pub(super) fn admit_module_boundary(&self, command: Command) -> Option<Command> {
        if let Command::ScopedRadio {
            command,
            right,
            producer,
        } = command
        {
            // The envelope is deliberately restricted to radio plans and their
            // reconnect/initialization steps. It cannot promote card/open/SMS.
            let eligible = matches!(
                &*command,
                Command::Radio { .. }
                    | Command::CallModeRadioInitialize { .. }
                    | Command::CallModeRadioReconnect { .. }
            );
            if eligible && right.is_active_for(&self.module_recovery, &producer) {
                return Some(*command);
            }
            return command.reject_new_module_work();
        }
        if !self.module_recovery.is_admission_revoked() {
            return Some(command);
        }
        command.reject_new_module_work()
    }
}

impl Command {
    fn reject_new_module_work(self) -> Option<Self> {
        macro_rules! reject {
            ($reply:expr) => {{
                let _ = $reply.send(Err(revoked()));
                None
            }};
        }
        match self {
            // This closure can only enter the ordinary command processor; its
            // contained command is checked again before physical acceptance.
            Command::LegacyGate(_)
            | Command::Read { .. }
            | Command::Close { .. }
            | Command::Finalize { .. }
            | Command::FencedClose { .. }
            | Command::RadioBinding { .. }
            | Command::SerialSmsObservation { .. }
            | Command::SerialSmsBindingCurrent { .. }
            | Command::CacheSerialSmsPhone { .. }
            | Command::ReadSmsGeneration { .. }
            | Command::ConfirmSmsGeneration { .. }
            | Command::FinishSmsReceive { .. } => Some(self),
            Command::Native(request) => match request {
                // Query/Finish require the currently refreshing ID and exact
                // native session in process_native_request. Commit publishes
                // cached readiness and performs no new physical I/O.
                NativeRequest::Query { .. }
                | NativeRequest::FinishMetadata { .. }
                | NativeRequest::Commit { .. }
                | NativeRequest::Close { .. } => Some(Command::Native(request)),
                NativeRequest::Claim { reply } => reject!(reply),
                NativeRequest::Open { reply, .. } => reject!(reply),
                NativeRequest::Initialize { reply, .. } => reject!(reply),
                NativeRequest::BeginMetadata { reply, .. } => reject!(reply),
            },
            Command::Card(request) => match request {
                // Command/End are restricted by the actor's exact card lease,
                // SIM generation, logical channel and command capability.
                CardRequest::Capture { .. }
                | CardRequest::Command { .. }
                | CardRequest::End { .. }
                | CardRequest::Module(ModuleRequest::Release { .. }) => {
                    Some(Command::Card(request))
                }
                CardRequest::Begin { reply, .. } | CardRequest::BeginSimIdentity { reply, .. } => {
                    reject!(reply)
                }
                CardRequest::Module(ModuleRequest::Retain { reply, .. }) => reject!(reply),
            },
            Command::ReadStoredSms {
                ref receive_fence, ..
            }
            | Command::DeleteArchivedSms {
                ref receive_fence, ..
            } if receive_fence.is_some() => Some(self),
            Command::Open { reply, .. } => reject!(reply),
            Command::DiscoverATPort { reply, .. } => reject!(reply),
            Command::Initialize { reply } | Command::CallModeRadioInitialize { reply, .. } => {
                reject!(reply)
            }
            Command::CallModeRadioReconnect { reply, .. } => reject!(reply),
            Command::Radio { reply, .. } => reject!(reply),
            Command::Write { reply, .. } => reject!(reply),
            Command::Execute { reply, .. } => reject!(reply),
            Command::SubmitSms { reply, .. } => reject!(reply),
            Command::DeleteArchivedSms { reply, .. } => reject!(reply),
            Command::ReadStoredSms { reply, .. } => reject!(reply),
            Command::ConfigureSmsReceive { enabled: false, .. } => Some(self),
            Command::ConfigureSmsReceive { reply, .. } | Command::ScanSmsReceive { reply } => {
                reject!(reply)
            }
            Command::NextSmsReceive { reply } => {
                let _ = reply.send(Ok(None));
                None
            }
            Command::ScopedRadio { command, .. } => command.reject_new_module_work(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_radio_transport::Action;
    use serialport::SerialPort as _;

    fn line(peer: &mut serialport::TTYPort) -> String {
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0];
            peer.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
            if byte[0] == b'\n' {
                return String::from_utf8(bytes).unwrap().trim().into();
            }
            assert!(bytes.len() < 256);
        }
    }
    fn request(
        right: Option<crate::module_recovery::OperationRight>,
        producer: &str,
        session: u64,
    ) -> (
        Command,
        Receiver<Result<crate::module_radio_transport::Outcome, ActorError>>,
    ) {
        let (reply, result) = mpsc::sync_channel(1);
        let command = Command::Radio {
            module_token: None,
            serial_session: session,
            action: Action::SetVerify { target: 4 },
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            commit_proof: None,
            reply,
        };
        (
            match right {
                Some(right) => Command::ScopedRadio {
                    command: Box::new(command),
                    right,
                    producer: producer.into(),
                },
                None => command,
            },
            result,
        )
    }
    #[test]
    fn scoped_serial_plan_finishes_after_revoke_but_expired_or_foreign_right_cannot_start() {
        let (mut peer, port) = serialport::TTYPort::pair().unwrap();
        peer.set_timeout(Duration::from_secs(2)).unwrap();
        let mut state = ActorState::new();
        state.port = Some(Box::new(port));
        state.serial_session = 41;
        state
            .cellular_suppressed
            .store(false, std::sync::atomic::Ordering::Release);
        let guard = state
            .module_recovery
            .admit_operation("finite-radio-fixture")
            .unwrap();
        let right = guard.right();
        state.module_recovery.revoke_for_tests();
        let opener: PortOpener = Box::new(|_, _| panic!("RF test cannot open hardware"));
        let (foreign, foreign_result) = request(Some(right.clone()), "different-producer", 41);
        state.process(foreign, &opener);
        assert_eq!(
            foreign_result.recv().unwrap().unwrap_err().code(),
            crate::module_recovery::CODE
        );
        let other_gate = crate::module_recovery::Gate::default();
        let foreign_guard = other_gate.admit_operation("finite-radio-fixture").unwrap();
        let (foreign, foreign_result) =
            request(Some(foreign_guard.right()), "finite-radio-fixture", 41);
        state.process(foreign, &opener);
        assert_eq!(
            foreign_result.recv().unwrap().unwrap_err().code(),
            crate::module_recovery::CODE
        );
        // A valid operation right still cannot act on a different session.
        let (stale, stale_result) = request(Some(right.clone()), "finite-radio-fixture", 40);
        state.process(stale, &opener);
        assert_eq!(
            stale_result.recv().unwrap().unwrap_err().code(),
            "serial_session_changed"
        );
        let (accepted, result) = request(Some(right.clone()), "finite-radio-fixture", 41);
        state.process(accepted, &opener);
        state.start_next_command();
        assert_eq!(line(&mut peer), "AT+CFUN?");
        for (response, next) in [
            (b"+CFUN: 1\r\nOK\r\n".as_slice(), "AT+CLCC"),
            (b"OK\r\n".as_slice(), "AT+CFUN=4"),
            (b"OK\r\n".as_slice(), "AT+CFUN?"),
        ] {
            state.frame_input(response).unwrap();
            state.complete_pending_command();
            state.start_next_command();
            assert_eq!(line(&mut peer), next);
        }
        state.frame_input(b"+CFUN: 4\r\nOK\r\n").unwrap();
        state.complete_pending_command();
        let outcome = result.recv().unwrap().unwrap();
        assert!(outcome.complete && outcome.verified && outcome.submitted);
        let (new, new_result) = request(None, "finite-radio-fixture", 41);
        state.process(new, &opener);
        assert_eq!(
            new_result.recv().unwrap().unwrap_err().code(),
            crate::module_recovery::CODE
        );
        drop(guard);
        let (expired, expired_result) = request(Some(right), "finite-radio-fixture", 41);
        state.process(expired, &opener);
        assert_eq!(
            expired_result.recv().unwrap().unwrap_err().code(),
            crate::module_recovery::CODE
        );
        let (reply, closed) = mpsc::sync_channel(1);
        state.process(
            Command::FencedClose {
                serial_session: 41,
                reply,
            },
            &opener,
        );
        closed.recv().unwrap().unwrap();
        assert!(state.port.is_none());
    }
}
