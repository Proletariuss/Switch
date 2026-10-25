//! Capture-boundary support only. No lease, registration or I/O worker is
//! started; service tests own a real Handle with seeded ordinary watch facts.
use super::*;

pub(crate) struct Facts {
    pub(crate) handle: Handle,
    pub(crate) commands: mpsc::Receiver<registered::Control>,
    reporter: Reporter,
    _phase: watch::Sender<Phase>,
    _stop: watch::Receiver<bool>,
}
impl Facts {
    pub(crate) fn registered(
        reader: &str,
        physical_generation: u64,
        generation: SessionGeneration,
    ) -> Self {
        let (reporter, status) = Reporter::new(Binding {
            reader: reader.into(),
            reader_generation: physical_generation,
            session_generation: generation,
            key: crate::native_vowifi_supervisor::ReaderSessionKey::new(
                reader,
                "894433000000000001",
            )
            .unwrap(),
        });
        let (controls, commands) = registered::controls();
        let (phase, phase_rx) = watch::channel(Phase::Registered);
        let (stop, stop_rx) = watch::channel(false);
        let handle = Handle {
            generation,
            controls,
            phase: phase_rx,
            status,
            reporter: reporter.clone(),
            stop,
            user_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let facts = Self {
            handle,
            commands,
            reporter,
            _phase: phase,
            _stop: stop_rx,
        };
        facts.update(|s| {
            s.phase = Phase::Registered;
            s.tunnel_started = true;
            s.tunnel_ready = true;
            s.register_started = true;
            s.registered = true;
            s.sms_route_ready = true;
            s.sms_ready = true;
            s.call_ready = true;
        });
        facts
    }
    pub(crate) fn update(&self, mutate: impl FnOnce(&mut Snapshot)) {
        let mut snapshot = self.handle.snapshot();
        mutate(&mut snapshot);
        self.reporter.set_test_snapshot(snapshot);
    }
    pub(crate) fn assert_no_command(&mut self) {
        assert!(
            self.commands.try_recv().is_err(),
            "capture/resolve submitted application work"
        );
    }
}
