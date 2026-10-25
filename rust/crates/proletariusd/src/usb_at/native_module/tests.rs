use super::*;
use std::sync::{
    Condvar,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Default)]
struct Trace {
    commands: Mutex<Vec<String>>,
    mode: AtomicUsize,
    fail: AtomicBool,
    closed: AtomicUsize,
    block_close: AtomicBool,
    close_error: AtomicBool,
    entered: tokio::sync::Notify,
    resume: Condvar,
    released: Mutex<bool>,
}
struct Port(Arc<Trace>);
impl Transport for Port {
    fn checked_close(
        self: Box<Self>,
    ) -> Result<crate::usb_at_transport::CloseDisposition, crate::usb_at_transport::TransportError>
    {
        // Explicit fixture release; Drop barriers must finish before success.
        let failed = self.0.close_error.load(Ordering::Acquire);
        drop(self);
        if failed {
            Err(usb_at_transport::close_unconfirmed())
        } else {
            Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
        }
    }

    fn description(&self) -> String {
        "retained native USB test".into()
    }
    fn command(
        &mut self,
        command: &str,
        _: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, crate::usb_at_transport::TransportError> {
        self.0.commands.lock().unwrap().push(command.into());
        if self.0.fail.load(Ordering::Acquire) {
            return Err(crate::usb_at_transport::TransportError {
                code: "usb_at_disconnected",
                message: "test disconnected".into(),
                response: String::new(),
                uncertain: true,
            });
        }
        if let Some(mode) = command.strip_prefix("AT+CFUN=") {
            self.0.mode.store(mode.parse().unwrap(), Ordering::Release);
        }
        Ok(if command == "AT+CFUN?" {
            format!("+CFUN: {}\r\nOK", self.0.mode.load(Ordering::Acquire))
        } else {
            "OK".into()
        })
    }
}
impl Drop for Port {
    fn drop(&mut self) {
        if self.0.block_close.load(Ordering::Acquire) {
            self.0.entered.notify_one();
            let mut released = self.0.released.lock().unwrap();
            while !*released {
                released = self.0.resume.wait(released).unwrap();
            }
        }
        self.0.closed.fetch_add(1, Ordering::AcqRel);
    }
}
impl Trace {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.resume.notify_all();
    }
}
async fn fixture() -> (
    Owner,
    crate::module_radio::Service,
    crate::module_radio::NativeVoWiFiIntent,
    Arc<Trace>,
    std::path::PathBuf,
) {
    let trace = Arc::new(Trace::default());
    trace.mode.store(1, Ordering::Release);
    let factory = trace.clone();
    let (usb, session) =
        Owner::native_module_fixture(Arc::new(move || Ok(Box::new(Port(factory.clone()))))).await;
    let root = std::env::temp_dir().join(format!(
        "native-usb-hold-{}",
        crate::snapshot::new_instance_id()
    ));
    let radio =
        crate::module_radio::Service::native_vowifi_fixture(usb.clone(), session, root.clone())
            .await;
    let intent = radio.native_vowifi_intent().unwrap();
    usb.retain_native_module(&intent).unwrap();
    (usb, radio, intent, trace, root)
}
#[tokio::test]
async fn native_hold_precedes_prepare_and_rejects_wire_close_until_real_restore() {
    let (usb, radio, intent, trace, root) = fixture().await;
    let (producer, session) = intent.usb_binding();
    let close = || CloseRequest {
        producer_id: producer.into(),
        session_id: session.into(),
        lease_id: String::new(),
    };
    assert_eq!(usb.close(close()).await.unwrap_err().code, "usb_at_busy");
    assert_eq!(trace.closed.load(Ordering::Acquire), 0);
    intent.prepare().await.unwrap();
    intent.enter().await.unwrap();
    assert_eq!(usb.close(close()).await.unwrap_err().code, "usb_at_busy");
    assert!(!intent.retire_confirmed_usb_loss().await.unwrap());
    intent.restore().await.unwrap();
    assert_eq!(trace.mode.load(Ordering::Acquire), 1);
    usb.release_native_module(&intent).unwrap();
    let before = trace.commands.lock().unwrap().len();
    assert_eq!(radio.native_vowifi_idle_mode().await.unwrap(), 1);
    assert!(
        trace.commands.lock().unwrap()[before..]
            .iter()
            .all(|command| command == "AT+CFUN?")
    );
    usb.close(close()).await.unwrap();
    assert_eq!(trace.closed.load(Ordering::Acquire), 1);
    radio.finish_shutdown().await.unwrap();
    usb.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn logical_generation_and_cancelled_replace_waiter_never_forge_closed_proof() {
    let (usb, radio, intent, trace, root) = fixture().await;
    intent.prepare().await.unwrap();
    intent.enter().await.unwrap();
    usb.inner.state.lock().unwrap().identity_generation += 1;
    assert!(!usb.native_module_closed(&intent));
    assert!(!intent.retire_confirmed_usb_loss().await.unwrap());
    assert!(intent.current());
    trace.block_close.store(true, Ordering::Release);
    let replacing = usb.clone();
    let waiter =
        tokio::spawn(async move { replacing.replace_producer("replacement-test".into()).await });
    trace.entered.notified().await;
    assert!(!usb.native_module_closed(&intent));
    assert_eq!(trace.closed.load(Ordering::Acquire), 0);
    assert!(!waiter.is_finished());
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    trace.release();
    // Join the retained physical revoke, rather than inferring completion from time.
    loop {
        let changed = usb.inner.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if usb.native_module_closed(&intent) {
            break;
        }
        changed.await;
    }
    assert_eq!(trace.closed.load(Ordering::Acquire), 1);
    let replacement = usb
        .open(
            OpenRequest {
                producer_id: "replacement-test".into(),
                lease_id: String::new(),
            },
            false,
        )
        .await
        .unwrap();
    assert_ne!(replacement["session_id"], intent.usb_binding().1);
    let before = trace.commands.lock().unwrap().clone();
    assert!(intent.retire_confirmed_usb_loss().await.unwrap());
    assert!(!intent.current());
    assert!(!radio.vowifi_owns_sim_channel());
    assert_eq!(
        *trace.commands.lock().unwrap(),
        before,
        "old loss retirement cannot send CFUN to replacement"
    );
    radio.finish_shutdown().await.unwrap();
    usb.shutdown().await.unwrap();
    assert_eq!(trace.closed.load(Ordering::Acquire), 2);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn fatal_command_waits_for_actual_drop_before_old_intent_retirement() {
    let (usb, radio, intent, trace, root) = fixture().await;
    intent.prepare().await.unwrap();
    intent.enter().await.unwrap();
    trace.block_close.store(true, Ordering::Release);
    trace.fail.store(true, Ordering::Release);
    let observing = intent.clone();
    let worker = tokio::spawn(async move { observing.validate().await });
    trace.entered.notified().await;
    assert!(!usb.native_module_closed(&intent));
    assert!(!worker.is_finished());
    assert!(intent.current());
    trace.release();
    assert!(worker.await.unwrap().is_err());
    assert_eq!(trace.closed.load(Ordering::Acquire), 1);
    assert!(usb.native_module_closed(&intent));
    let before = trace.commands.lock().unwrap().clone();
    assert!(intent.retire_confirmed_usb_loss().await.unwrap());
    assert_eq!(*trace.commands.lock().unwrap(), before);
    assert!(!radio.vowifi_owns_sim_channel());
    radio.finish_shutdown().await.unwrap();
    usb.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn checked_release_error_never_confirms_native_hold_after_replacement_or_absent_port() {
    let (usb, _radio, intent, trace, root) = fixture().await;
    trace.close_error.store(true, Ordering::Release);
    assert_eq!(
        usb.replace_producer("replacement-close-error".into())
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(trace.closed.load(Ordering::Acquire), 1);
    assert!(!usb.native_module_closed(&intent));
    assert_eq!(
        usb.release_native_module(&intent).unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        usb.capture_voice_target("replacement-close-error")
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        usb.replace_producer("replacement-close-error-again".into())
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    assert!(!usb.native_module_closed(&intent));
    assert_eq!(
        usb.release_native_module(&intent).unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        usb.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    std::fs::remove_dir_all(root).unwrap();
}
