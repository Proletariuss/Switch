//! Ownership-only transports for actor tests. Synthetic anchors cannot enter
//! the native successor opener and make no physical-device identity claim.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(crate) const IMEI: &str = "123456789012345";
pub(crate) struct Trace {
    pub(crate) commands: Mutex<Vec<String>>,
    pub(crate) drops: AtomicUsize,
    pub(crate) anchor_enabled: AtomicBool,
    pub(crate) reboot_uncertain: AtomicBool,
    pub(crate) close_disconnected: AtomicBool,
    pub(crate) close_error: AtomicBool,
    pub(crate) firmware_broken: AtomicBool,
    pub(crate) imei: Mutex<String>,
    pub(crate) before_command: Mutex<Option<Box<dyn Fn(&str) + Send + Sync>>>,
    voice: Mutex<(u8, u8)>,
}
impl Default for Trace {
    fn default() -> Self {
        Self {
            commands: Mutex::new(Vec::new()),
            drops: AtomicUsize::new(0),
            anchor_enabled: AtomicBool::new(true),
            reboot_uncertain: AtomicBool::new(false),
            close_disconnected: AtomicBool::new(false),
            close_error: AtomicBool::new(false),
            firmware_broken: AtomicBool::new(false),
            imei: Mutex::new(IMEI.into()),
            before_command: Mutex::new(None),
            voice: Mutex::new((0, 1)),
        }
    }
}
pub(crate) struct TestTransport {
    pub(crate) target: usb_at_transport::PhysicalTarget,
    pub(crate) trace: Arc<Trace>,
}
impl Transport for TestTransport {
    fn checked_close(
        self: Box<Self>,
    ) -> Result<crate::usb_at_transport::CloseDisposition, crate::usb_at_transport::TransportError>
    {
        // Explicit fixture release; Drop barriers must finish before success.
        let disconnected = self.trace.close_disconnected.load(Ordering::Acquire);
        let failed = self.trace.close_error.load(Ordering::Acquire);
        drop(self);
        if failed {
            return Err(usb_at_transport::close_unconfirmed());
        }
        Ok(if disconnected {
            CloseDisposition::DisconnectedAndHandleDropped
        } else {
            CloseDisposition::ReleasedAndHandleDropped
        })
    }

    fn physical_target(&self) -> Result<usb_at_transport::PhysicalTarget, TransportError> {
        Ok(self.target.clone())
    }
    fn capture_reenumeration_anchor(
        &mut self,
        _: Instant,
        guard: crate::module_radio::CallAdmission,
    ) -> Result<usb_at_transport::ReenumerationAnchor, TransportError> {
        if !guard() || !self.trace.anchor_enabled.load(Ordering::Acquire) {
            return Err(TransportError {
                code: "usb_at_target_unconfirmed",
                message: "Test transport has no reconnect anchor".into(),
                response: String::new(),
                uncertain: false,
            });
        }
        Ok(usb_at_transport::ReenumerationAnchor::synthetic_for_tests(
            self.target.clone(),
            (0x2c7c, 0x0125),
        ))
    }
    fn command(
        &mut self,
        command: &str,
        _: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        self.trace.commands.lock().unwrap().push(command.into());
        if let Some(hook) = self.trace.before_command.lock().unwrap().as_ref() {
            hook(command);
        }
        let mut voice = self.trace.voice.lock().unwrap();
        let response = match command {
            "AT+CGSN" => format!("{}\r\nOK\r\n", self.trace.imei.lock().unwrap()),
            "ATI" if self.trace.firmware_broken.load(Ordering::Acquire) => {
                return Err(TransportError {
                    code: "usb_at_disconnected",
                    message: "Broken test firmware stream".into(),
                    response: String::new(),
                    uncertain: true,
                });
            }
            "ATI" => "test firmware\r\nOK\r\n".into(),
            "AT+CLCC" => "\r\nOK\r\n".into(),
            "AT+CFUN?" => "+CFUN: 1\r\nOK\r\n".into(),
            "AT+QCFG=\"ims\"" => format!("+QCFG: \"ims\",{},1\r\nOK\r\n", voice.0),
            "AT+QCFG=\"volte_disable\"" => {
                format!("+QCFG: \"volte_disable\",{}\r\nOK\r\n", voice.1)
            }
            "AT+QCFG=\"usbcfg\"" => {
                "+QCFG: \"usbcfg\",0x2C7C,0x2400,0,0,0,0,0,0,0\r\nOK\r\n".into()
            }
            "AT+QCFG=\"ims\",1" => {
                voice.0 = 1;
                "\r\nOK\r\n".into()
            }
            "AT+QCFG=\"ims\",0" => {
                voice.0 = 0;
                "\r\nOK\r\n".into()
            }
            "AT+QCFG=\"volte_disable\",0" => {
                voice.1 = 0;
                "\r\nOK\r\n".into()
            }
            "AT+QCFG=\"volte_disable\",1" => {
                voice.1 = 1;
                "\r\nOK\r\n".into()
            }
            "AT+CFUN=1,1" if self.trace.reboot_uncertain.load(Ordering::Acquire) => {
                return Err(TransportError {
                    code: "usb_at_disconnected",
                    message: "Test reboot disconnected after submission".into(),
                    response: String::new(),
                    uncertain: true,
                });
            }
            "AT+CFUN=1,1" => "\r\nOK\r\n".into(),
            _ => panic!("unexpected ownership-fixture command: {command}"),
        };
        Ok(response)
    }
    fn description(&self) -> String {
        "ownership-only USB fixture".into()
    }
}
impl Drop for TestTransport {
    fn drop(&mut self) {
        self.trace.drops.fetch_add(1, Ordering::AcqRel);
    }
}

pub(crate) fn install(
    owner: &Owner,
    session: &str,
    target: usb_at_transport::PhysicalTarget,
    trace: Arc<Trace>,
) {
    *owner.inner.port.lock().unwrap() = Some(Box::new(TestTransport { target, trace }));
    let mut state = owner.inner.state.lock().unwrap();
    assert!(!state.busy);
    state.identity_generation += 1;
    state.session = session.into();
    state.port_identity = Some(PortIdentity::current(&state));
    state.reconnect = None;
    state.cellular_suppressed = false;
}

pub(crate) fn install_default(owner: &Owner, session: &str) -> Arc<Trace> {
    let target = usb_at_transport::PhysicalTarget::synthetic_for_tests();
    let trace = Arc::new(Trace::default());
    install(owner, session, target.clone(), trace.clone());
    let returned = trace.clone();
    set_opener(
        owner,
        Arc::new(move |source, anchor, _, guard| {
            assert!(source.same_connection(&target));
            if let Some(anchor) = anchor {
                assert!(anchor.same_source(source));
            }
            assert!(guard());
            Ok(Box::new(TestTransport {
                target: target.clone(),
                trace: returned.clone(),
            }))
        }),
    );
    trace
}

pub(crate) fn set_opener(owner: &Owner, opener: super::reconnect::TestOpener) {
    *owner.inner.reconnect_opener.lock().unwrap() = Some(opener);
}
pub(crate) fn current_session(owner: &Owner) -> String {
    owner.inner.state.lock().unwrap().session.clone()
}

pub(crate) fn last_close_disposition(owner: &Owner) -> Option<CloseDisposition> {
    owner
        .inner
        .state
        .lock()
        .unwrap()
        .last_close_result
        .as_ref()
        .and_then(|(_, result)| result.as_ref().ok().copied())
}
