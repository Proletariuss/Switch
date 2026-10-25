use super::*;

#[derive(Default)]
struct Trace {
    replies: Mutex<std::collections::BTreeMap<String, String>>,
    commands: Mutex<Vec<String>>,
}
struct Port(Arc<Trace>);
impl Transport for Port {
    fn checked_close(
        self: Box<Self>,
    ) -> Result<crate::usb_at_transport::CloseDisposition, crate::usb_at_transport::TransportError>
    {
        // Explicit fixture release; Drop barriers must finish before success.
        drop(self);
        Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
    }

    fn description(&self) -> String {
        "Native SMS identity fixture".into()
    }
    fn command(
        &mut self,
        command: &str,
        _: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        self.0.commands.lock().unwrap().push(command.into());
        if let Some(reply) = self.0.replies.lock().unwrap().get(command) {
            return Ok(reply.clone());
        }
        Ok(match command {
            "AT+CFUN?" => "+CFUN: 1\r\nOK\r\n",
            "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n",
            "AT+QCCID" => "+QCCID: 8901000000000000000\r\nOK\r\n",
            "AT+CIMI" => "001010000000001\r\nOK\r\n",
            _ => "OK\r\n",
        }
        .into())
    }
}
async fn fixture() -> (Owner, Arc<Trace>, String, u64) {
    let trace = Arc::new(Trace::default());
    let t = trace.clone();
    let (owner, session) =
        Owner::native_module_fixture(Arc::new(move || Ok(Box::new(Port(t.clone()))))).await;
    owner.native_device_observation(&session).await.unwrap();
    let (_, generation) = owner.capture_native_sms(&session).await.unwrap();
    assert!(owner.cached_device_observation().is_some());
    assert!(owner.native_sms_binding_current(&session, generation));
    (owner, trace, session, generation)
}
#[tokio::test]
async fn actual_not_ready_absent_or_invalid_identity_revokes_old_prepared_facts_and_display() {
    for (command, reply) in [
        ("AT+CPIN?", "+CPIN: SIM PIN\r\nOK\r\n"),
        ("AT+CPIN?", "+CME ERROR: 10\r\n"),
        ("AT+CPIN?", "+CME ERROR: SIM not inserted\r\n"),
        ("AT+QCCID", "+QCCID: \"\"\r\nOK\r\n"),
        ("AT+CIMI", "OK\r\n"),
    ] {
        let (owner, trace, session, generation) = fixture().await;
        trace
            .replies
            .lock()
            .unwrap()
            .insert(command.into(), reply.into());
        assert!(
            owner.capture_native_sms(&session).await.is_err(),
            "{command}: {reply}"
        );
        assert!(!owner.native_sms_binding_current(&session, generation));
        assert!(owner.cached_device_observation().is_none());
        trace.replies.lock().unwrap().clear();
        let (_, next) = owner.capture_native_sms(&session).await.unwrap();
        assert!(next > generation);
        assert!(
            !owner.native_sms_binding_current(&session, generation),
            "old prepared work cannot revive after card recovery"
        );
        assert!(
            !trace
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|v| v.starts_with("AT+CMGS"))
        );
    }
}
#[tokio::test]
async fn observed_changed_qccid_remains_disproved_when_later_imsi_read_fails() {
    let (owner, trace, session, generation) = fixture().await;
    trace.replies.lock().unwrap().extend([
        (
            "AT+QCCID".into(),
            "+QCCID: 8901000000000000001\r\nOK\r\n".into(),
        ),
        ("AT+CIMI".into(), "ERROR\r\n".into()),
    ]);
    assert!(owner.capture_native_sms(&session).await.is_err());
    assert!(!owner.native_sms_binding_current(&session, generation));
    assert!(owner.cached_device_observation().is_none());
}
#[tokio::test]
async fn busy_or_generic_failed_probe_does_not_invent_a_card_change() {
    let (owner, trace, session, generation) = fixture().await;
    owner
        .inner
        .state
        .lock()
        .unwrap()
        .inhibits
        .insert("test".into(), 1);
    let count = trace.commands.lock().unwrap().len();
    assert!(owner.capture_native_sms(&session).await.is_err());
    assert_eq!(trace.commands.lock().unwrap().len(), count);
    owner.inner.state.lock().unwrap().inhibits.clear();
    assert!(owner.native_sms_binding_current(&session, generation));
    trace
        .replies
        .lock()
        .unwrap()
        .insert("AT+CPIN?".into(), "ERROR\r\n".into());
    assert!(owner.capture_native_sms(&session).await.is_err());
    assert!(owner.native_sms_binding_current(&session, generation));
    assert!(owner.cached_device_observation().is_some());
    trace.replies.lock().unwrap().clear();
    assert_eq!(
        owner.capture_native_sms(&session).await.unwrap().1,
        generation
    );
}
