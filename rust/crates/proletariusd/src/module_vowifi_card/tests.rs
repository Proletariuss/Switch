use super::*;
use crate::usb_at_transport::{Transport, TransportError};
use std::{
    sync::{Condvar, atomic::AtomicUsize},
    time::Duration,
};
use tokio::sync::Notify;
#[derive(Default)]
pub(crate) struct Block {
    pub(crate) entered: Notify,
    released: Mutex<bool>,
    resume: Condvar,
}
impl Block {
    fn wait(&self) {
        self.entered.notify_one();
        let mut ready = self.released.lock().unwrap();
        while !*ready {
            ready = self.resume.wait(ready).unwrap();
        }
    }
    pub(crate) fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.resume.notify_all();
    }
}
pub(crate) struct Trace {
    pub(crate) commands: Mutex<Vec<String>>,
    pub(crate) mode: AtomicUsize,
    pub(crate) changed: AtomicBool,
    pub(crate) auth: AtomicUsize,
    pub(crate) failed_restore: AtomicBool,
    pub(crate) blocked: Mutex<Option<(String, Arc<Block>)>>,
}
impl Default for Trace {
    fn default() -> Self {
        Self {
            commands: Mutex::new(vec![]),
            mode: AtomicUsize::new(1),
            changed: AtomicBool::new(false),
            auth: AtomicUsize::new(0),
            failed_restore: AtomicBool::new(false),
            blocked: Mutex::new(None),
        }
    }
}
struct Sim {
    trace: Arc<Trace>,
    selected: [u8; 2],
}
impl Sim {
    fn apdu(&mut self, c: &[u8]) -> Vec<u8> {
        let mut result = match c[1] {
            0xa4 => {
                if c.len() >= 7 && c[2] != 4 {
                    self.selected = [c[5], c[6]];
                }
                vec![]
            }
            0xb2 if self.selected == [0x2f, 0] && c[2] == 1 => vec![
                0x61, 11, 0x4f, 9, 0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff, 1, 0x61, 11, 0x4f, 9, 0xa0,
                0, 0, 0, 0x87, 0x10, 4, 0xff, 1,
            ],
            0xb2 => return vec![0x6a, 0x83],
            0xb0 => match self.selected {
                [0x6f, 0xad] => vec![0, 0, 0, 2],
                [0x6f, 7] if c[4] == 1 => vec![8],
                [0x6f, 7] => vec![8, 0x29, 0x43, 0x33, 0x10, 0x32, 0x54, 0x76, 0x98],
                [0x2f, 0xe2] => vec![
                    0x98,
                    0x10,
                    0x32,
                    0x54,
                    0x76,
                    0x98,
                    0x10,
                    0x32,
                    0x54,
                    if self.trace.changed.load(Ordering::Acquire) {
                        0xf7
                    } else {
                        0xf6
                    },
                ],
                _ => return vec![0x6a, 0x82],
            },
            0x88 => {
                self.trace.auth.fetch_add(1, Ordering::SeqCst);
                let mut v = vec![0xdb, 8];
                let ike = c.windows(16).any(|v| v == [0x11; 16]);
                v.extend(if ike {
                    vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
                } else {
                    vec![1; 8]
                });
                v.push(16);
                v.extend([if ike { 0x31 } else { 2 }; 16]);
                v.push(16);
                v.extend([if ike { 0x42 } else { 3 }; 16]);
                v
            }
            _ => return vec![0x6a, 0x81],
        };
        result.extend([0x90, 0]);
        result
    }
}
impl Transport for Sim {
    fn checked_close(
        self: Box<Self>,
    ) -> Result<crate::usb_at_transport::CloseDisposition, crate::usb_at_transport::TransportError>
    {
        // Explicit fixture release; Drop barriers must finish before success.
        drop(self);
        Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
    }

    fn command(
        &mut self,
        c: &str,
        _: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        self.trace.commands.lock().unwrap().push(c.into());
        let block = {
            let mut b = self.trace.blocked.lock().unwrap();
            if b.as_ref().is_some_and(|(prefix, _)| {
                c.starts_with(prefix)
                    || prefix == "@AUTH"
                        && c.split('"')
                            .nth(1)
                            .is_some_and(|hex| hex.get(2..4) == Some("88"))
            }) {
                b.take().map(|(_, b)| b)
            } else {
                None
            }
        };
        if let Some(block) = block {
            block.wait();
        }
        match c {
            "AT+CFUN?" => Ok(format!(
                "+CFUN: {}\r\nOK",
                self.trace.mode.load(Ordering::Acquire)
            )),
            "AT+CFUN=4" => {
                self.trace.mode.store(4, Ordering::Release);
                Ok("OK".into())
            }
            "AT+CFUN=1" => {
                if self.trace.failed_restore.load(Ordering::Acquire) {
                    return Ok("ERROR".into());
                }
                self.trace.mode.store(1, Ordering::Release);
                Ok("OK".into())
            }
            "AT+CLCC" => Ok("OK".into()),
            "AT+CNUM" => Ok("OK".into()),
            "AT+CSCA?" => Ok("+CSCA: \"+447802002606\",145\r\nOK".into()),
            "AT+QCCID" => Ok(format!(
                "+QCCID: 890123456789012345{}\r\nOK",
                if self.trace.changed.load(Ordering::Acquire) {
                    7
                } else {
                    6
                }
            )),
            "AT+CIMI" => Ok("234330123456789\r\nOK".into()),
            _ if c.starts_with("AT+CSIM=") => {
                let hex = c.split('"').nth(1).unwrap();
                let apdu = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                    .collect::<Vec<_>>();
                let response = self.apdu(&apdu);
                let hex = response
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<String>();
                Ok(format!("+CSIM: {},\"{}\"\r\nOK", hex.len(), hex))
            }
            _ => panic!("unexpected physical command {c}"),
        }
    }
    fn description(&self) -> String {
        "module fixture".into()
    }
}
struct Fixture {
    card: Arc<ModuleSessionCard>,
    trace: Arc<Trace>,
    usb: Owner,
    radio: Service,
    stop: watch::Sender<bool>,
    root: std::path::PathBuf,
}
impl Fixture {
    async fn new(block: Option<(String, Arc<Block>)>) -> Self {
        let trace = Arc::new(Trace::default());
        *trace.blocked.lock().unwrap() = block;
        let copy = trace.clone();
        let (usb, session) = Owner::native_module_fixture(Arc::new(move || {
            Ok(Box::new(Sim {
                trace: copy.clone(),
                selected: [0, 0],
            }))
        }))
        .await;
        let root = std::env::temp_dir().join(format!(
            "module-card-{}",
            crate::snapshot::new_instance_id()
        ));
        let radio = Service::native_vowifi_fixture(usb.clone(), session, root.clone()).await;
        let (stop, rx) = watch::channel(false);
        let card = ModuleSessionCard::begin_prepare(usb.clone(), radio.clone(), rx);
        Self {
            card,
            trace,
            usb,
            radio,
            stop,
            root,
        }
    }
    async fn finish(self) {
        self.card.clone().release_with_confirmation().await.unwrap();
        self.radio.finish_shutdown().await.unwrap();
        self.usb.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
#[tokio::test]
async fn real_identity_aka_rf_restore_and_cached_status() {
    let f = Fixture::new(None).await;
    f.card.wait_ready().await.unwrap();
    assert!(f.card.is_current());
    assert_eq!(f.card.status().cfun_mode, Some(4));
    assert_eq!(f.card.status().original_mode, Some(1));
    let count = f.trace.commands.lock().unwrap().len();
    let _ = f.card.status();
    assert_eq!(f.trace.commands.lock().unwrap().len(), count);
    f.card.observe_radio().await.unwrap();
    assert!(
        f.trace.commands.lock().unwrap()[count..]
            .iter()
            .all(|c| c == "AT+CFUN?")
    );
    f.card.validate_business().await.unwrap();
    assert!(matches!(
        f.card
            .clone()
            .authenticate_retained(ReaderAkaApplication::Usim, [1; 16], [2; 16])
            .await
            .unwrap(),
        SimAkaResult::Success { .. }
    ));
    f.card.clone().release_with_confirmation().await.unwrap();
    assert!(!f.card.is_current());
    assert_eq!(f.trace.mode.load(Ordering::Acquire), 1);
    assert!(f.card.status().cleanup_confirmed);
    f.finish().await;
}
#[tokio::test]
async fn cancellation_waits_for_submitted_identity_before_confirmed_cleanup() {
    let block = Arc::new(Block::default());
    let f = Fixture::new(Some(("AT+CNUM".into(), block.clone()))).await;
    tokio::time::timeout(Duration::from_secs(10), block.entered.notified())
        .await
        .expect("physical command entered");
    f.stop.send_replace(true);
    let card = f.card.clone();
    let release = tokio::spawn(async move { card.release_with_confirmation().await });
    tokio::task::yield_now().await;
    assert!(!release.is_finished());
    assert!(!f.card.status().cleanup_confirmed);
    assert!(f.radio.vowifi_owns_sim_channel());
    block.release();
    assert!(f.card.wait_ready().await.is_err());
    release.await.unwrap().unwrap();
    assert!(!f.radio.vowifi_owns_sim_channel());
    f.finish().await;
}
#[tokio::test]
async fn restore_failure_retains_exact_owner_until_retry_succeeds() {
    let f = Fixture::new(None).await;
    f.card.wait_ready().await.unwrap();
    f.trace.failed_restore.store(true, Ordering::Release);
    assert!(f.card.clone().release_with_confirmation().await.is_err());
    assert!(!f.card.status().cleanup_confirmed);
    assert!(f.radio.vowifi_owns_sim_channel());
    assert!(f.radio.native_vowifi_intent().is_err());
    f.trace.failed_restore.store(false, Ordering::Release);
    f.card.clone().release_with_confirmation().await.unwrap();
    assert_eq!(f.trace.mode.load(Ordering::Acquire), 1);
    assert!(!f.radio.vowifi_owns_sim_channel());
    f.finish().await;
}
#[tokio::test]
async fn changed_profile_and_rf_mode_reject_new_business() {
    let f = Fixture::new(None).await;
    f.card.wait_ready().await.unwrap();
    f.trace.changed.store(true, Ordering::Release);
    assert!(f.card.validate_business().await.is_err());
    assert!(!f.card.is_current());
    assert!(
        f.card
            .clone()
            .authenticate_retained(ReaderAkaApplication::Usim, [1; 16], [2; 16])
            .await
            .is_err()
    );
    assert_eq!(f.trace.auth.load(Ordering::Acquire), 0);
    f.finish().await;
    let f = Fixture::new(None).await;
    f.card.wait_ready().await.unwrap();
    f.trace.mode.store(1, Ordering::Release);
    assert!(f.card.observe_radio().await.is_err());
    assert!(!f.card.is_current());
    f.finish().await;
}

#[tokio::test]
async fn abandoned_auth_waiter_does_not_release_rf_before_actual_auth_completion() {
    let f = Fixture::new(None).await;
    f.card.wait_ready().await.unwrap();
    let block = Arc::new(Block::default());
    *f.trace.blocked.lock().unwrap() = Some(("@AUTH".into(), block.clone()));
    let card = f.card.clone();
    let caller = tokio::spawn(async move {
        card.authenticate_retained(ReaderAkaApplication::Isim, [1; 16], [2; 16])
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), block.entered.notified())
        .await
        .expect("physical command entered");
    caller.abort();
    let _ = caller.await;
    let card = f.card.clone();
    let release = tokio::spawn(async move { card.release_with_confirmation().await });
    tokio::task::yield_now().await;
    assert!(!release.is_finished());
    assert_eq!(f.trace.mode.load(Ordering::Acquire), 4);
    assert!(f.radio.vowifi_owns_sim_channel());
    assert!(!f.card.status().cleanup_confirmed);
    block.release();
    release.await.unwrap().unwrap();
    assert_eq!(f.trace.auth.load(Ordering::Acquire), 1);
    assert_eq!(f.trace.mode.load(Ordering::Acquire), 1);
    f.finish().await;
}
#[tokio::test]
async fn old_usb_identity_generation_is_rejected_without_further_io() {
    let f = Fixture::new(None).await;
    f.card.wait_ready().await.unwrap();
    let intent = f.card.intent.get().unwrap();
    let replacement = f.usb.native_module_identity(intent).await.unwrap();
    assert_ne!(replacement.generation(), f.card.generation());
    assert!(!f.card.is_current());
    let before = f.trace.commands.lock().unwrap().len();
    assert!(
        f.card
            .clone()
            .authenticate_retained(ReaderAkaApplication::Usim, [1; 16], [2; 16])
            .await
            .is_err()
    );
    assert_eq!(f.trace.commands.lock().unwrap().len(), before);
    assert!(!f.card.status().restoration_skipped_reader_lost);
    f.finish().await;
}

#[tokio::test]
async fn physically_closed_usb_releases_card_with_explicit_unrestored_receipt() {
    let f = Fixture::new(None).await;
    f.card.wait_ready().await.unwrap();
    f.usb
        .replace_producer("replacement-test".into())
        .await
        .unwrap();
    assert!(!f.card.is_current());
    let before = f.trace.commands.lock().unwrap().clone();
    f.card.clone().release_with_confirmation().await.unwrap();
    let status = f.card.status();
    assert!(status.cleanup_confirmed);
    assert!(status.restoration_skipped_reader_lost);
    assert_eq!(status.phase, "released_reader_lost");
    assert_eq!(status.cfun_mode, None);
    assert_eq!(status.code, Some("module_vowifi_radio_lost"));
    assert_eq!(
        f.trace.mode.load(Ordering::Acquire),
        4,
        "closed device was not reported as RF restored"
    );
    assert_eq!(
        *f.trace.commands.lock().unwrap(),
        before,
        "loss cleanup cannot access replacement USB"
    );
    assert!(!f.radio.vowifi_owns_sim_channel());
    f.finish().await;
}

#[tokio::test]
async fn abandoned_readiness_waiter_keeps_retained_preparation_alive() {
    let block = Arc::new(Block::default());
    let f = Fixture::new(Some(("AT+CNUM".into(), block.clone()))).await;
    tokio::time::timeout(Duration::from_secs(10), block.entered.notified())
        .await
        .expect("physical identity command entered");
    let card = f.card.clone();
    let waiter = tokio::spawn(async move { card.wait_ready().await });
    tokio::task::yield_now().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(f.radio.vowifi_owns_sim_channel());
    assert!(!*f.card.lost_receiver().borrow());
    block.release();
    f.card.wait_ready().await.unwrap();
    assert!(f.card.is_current());
    assert_eq!(
        f.card.usb_session_id(),
        Some(match f.card.binding.get().unwrap() {
            ModuleIdentity::Usb(v) => v.session_id(),
            ModuleIdentity::Serial(_) => unreachable!(),
        })
    );
    assert_eq!(f.trace.mode.load(Ordering::Acquire), 4);
    f.finish().await;
}

/// Reuse the actual USB command worker and SIM APDU emulator for Service composition.
pub(crate) async fn service_fixture(root: std::path::PathBuf) -> (Owner, Service, Arc<Trace>) {
    let trace = Arc::new(Trace::default());
    let copy = trace.clone();
    let (usb, session) = Owner::native_module_fixture(Arc::new(move || {
        Ok(Box::new(Sim {
            trace: copy.clone(),
            selected: [0, 0],
        }))
    }))
    .await;
    let radio = Service::native_vowifi_fixture(usb.clone(), session, root).await;
    (usb, radio, trace)
}

#[tokio::test]
async fn revoke_after_exact_hold_before_prepare_admission_releases_unused_hold_without_rf() {
    let root = std::env::temp_dir().join(format!(
        "module-card-unadmitted-{}",
        crate::snapshot::new_instance_id()
    ));
    let (usb, radio, trace) = service_fixture(root.clone()).await;
    let gate = radio.module_recovery().clone();
    let (_stop, stopped) = watch::channel(false);
    let card = ModuleSessionCard::begin_prepare(usb.clone(), radio.clone(), stopped);
    // This current-thread fixture cannot run the spawned preparation before
    // the first await. Revoke at the production worker's exact retained hold.
    *card.revoke_after_hold.lock().unwrap() = Some(gate);
    assert!(card.wait_ready().await.is_err());
    assert!(card.physical_held.load(Ordering::Acquire));
    assert!(card.prepare_not_admitted.load(Ordering::Acquire));
    card.clone().release_with_confirmation().await.unwrap();
    assert!(card.status().cleanup_confirmed);
    assert!(!radio.vowifi_owns_sim_channel());
    assert!(
        trace.commands.lock().unwrap().is_empty(),
        "unadmitted preparation must perform no RF or SIM command"
    );
    assert_eq!(trace.mode.load(Ordering::Acquire), 1);
    radio.finish_shutdown().await.unwrap();
    usb.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn accepted_rf_work_revoked_during_write_keeps_real_restore_and_hold_release() {
    let block = Arc::new(Block::default());
    let fixture = Fixture::new(Some(("AT+CFUN=4".into(), block.clone()))).await;
    block.entered.notified().await;
    fixture.radio.module_recovery().revoke_for_tests();
    assert!(!fixture.card.prepare_not_admitted.load(Ordering::Acquire));
    block.release();
    fixture.card.wait_ready().await.unwrap();
    fixture
        .card
        .clone()
        .release_with_confirmation()
        .await
        .unwrap();
    assert!(fixture.card.status().cleanup_confirmed);
    assert_eq!(fixture.trace.mode.load(Ordering::Acquire), 1);
    assert!(
        fixture
            .trace
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| command == "AT+CFUN=1")
    );
    assert!(!fixture.radio.vowifi_owns_sim_channel());
    fixture.finish().await;
}

#[tokio::test]
async fn late_new_sim_transaction_rejected_after_prepare_still_releases_registered_intent_and_hold()
{
    let root = std::env::temp_dir().join(format!(
        "module-card-late-sim-{}",
        crate::snapshot::new_instance_id()
    ));
    let (usb, radio, trace) = service_fixture(root.clone()).await;
    let gate = radio.module_recovery().clone();
    let (_stop, stopped) = watch::channel(false);
    let card = ModuleSessionCard::begin_prepare(usb.clone(), radio.clone(), stopped);
    *card.revoke_after_prepare.lock().unwrap() = Some(gate);
    assert!(card.wait_ready().await.is_err());
    assert!(
        !card.prepare_not_admitted.load(Ordering::Acquire),
        "prepare really was admitted"
    );
    card.clone().release_with_confirmation().await.unwrap();
    assert!(card.status().cleanup_confirmed);
    assert!(!radio.vowifi_owns_sim_channel());
    assert!(
        trace.commands.lock().unwrap().is_empty(),
        "new SIM transaction is denied before APDU and enter"
    );
    assert_eq!(trace.mode.load(Ordering::Acquire), 1);
    radio.finish_shutdown().await.unwrap();
    usb.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
