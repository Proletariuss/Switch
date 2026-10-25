//! Isolated allocator archive only: each tested media owner claims its own
//! worker plan, then opens the allocator TLS on that actual codec thread.
use super::*;
use proletarius_call::evs::{
    CodecError,
    fault_test_support::{FaultScope, report},
};
use proletarius_vowifi::{
    TunnelConfiguration, netstack_runtime::TunnelNetwork, netstack_udp::UdpSocketRegistry,
};
use smoltcp::iface::SocketSet;
use std::sync::{
    Condvar,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

tokio::task_local! { static CURRENT_PLAN: Arc<FaultPlan>; }
#[derive(Default)]
pub(super) struct FaultPlan {
    state: Mutex<PlanState>,
}
#[derive(Default)]
struct PlanState {
    workers: Vec<Arc<WorkerRecord>>,
    fail_worker: Option<(usize, u64)>,
    close_ready_worker: Option<usize>,
    pause_exit_worker: Option<usize>,
}
pub(super) struct WorkerRecord {
    profile: CodecProfile,
    token: AtomicU64,
    exit_entered: AtomicBool,
    exited: AtomicBool,
    changed: tokio::sync::Notify,
    release: (Mutex<bool>, Condvar),
    native: Mutex<Option<std::sync::Weak<CodecNative>>>,
}
pub(super) struct WorkerProbe {
    record: Arc<WorkerRecord>,
    fail_at: u64,
    close_ready: bool,
}
impl FaultPlan {
    fn worker(&self, index: usize) -> Arc<WorkerRecord> {
        self.state.lock().unwrap().workers[index - 1].clone()
    }
    async fn wait_worker(&self, index: usize) -> Arc<WorkerRecord> {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(record) = self.state.lock().unwrap().workers.get(index - 1).cloned() {
                    break record;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("actual native worker claimed its owner-local plan")
    }
    fn len(&self) -> usize {
        self.state.lock().unwrap().workers.len()
    }
    fn arm(&self, worker: usize, fail_at: Option<u64>, close_ready: bool, pause_exit: bool) {
        let mut state = self.state.lock().unwrap();
        state.fail_worker = fail_at.map(|at| (worker, at));
        state.close_ready_worker = close_ready.then_some(worker);
        state.pause_exit_worker = pause_exit.then_some(worker);
    }
    fn assert_drained(&self) {
        let global = proletarius_call::evs::fault_test_support::global_report();
        assert_eq!(
            (global.system_failures, global.tracking_errors),
            (0, 0),
            "isolated Rust fault process must not hide real OOM/foreign/double free: {global:?}"
        );
        for worker in &self.state.lock().unwrap().workers {
            assert!(
                worker.exited.load(Ordering::SeqCst),
                "actual codec thread must have exited"
            );
            let facts = report(worker.token.load(Ordering::SeqCst)).unwrap();
            assert_eq!(
                (facts.live_blocks, facts.live_bytes),
                (0, 0),
                "native constructor allocations must be released: {facts:?}"
            );
            assert_eq!(facts.system_failures, 0, "no real OOM is induced");
            assert_eq!(facts.tracking_errors, 0);
        }
    }
}
impl WorkerRecord {
    fn action(&self, action: impl FnOnce() -> bool + Send + 'static) {
        let native = self
            .native
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .upgrade()
            .expect("actual worker still owned");
        assert!(
            native
                .sender
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .try_send(CodecCommand::TestAction(Box::new(action)))
                .is_ok()
        );
    }
    async fn wait_exit_gate(&self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let changed = self.changed.notified();
                if self.exit_entered.load(Ordering::SeqCst) {
                    break;
                }
                changed.await;
            }
        })
        .await
        .expect("actual codec thread exit gate");
    }
    fn release_exit(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
    }
}
impl WorkerProbe {
    pub(super) fn close_ready(&self) -> bool {
        self.close_ready
    }
}
impl Drop for WorkerProbe {
    fn drop(&mut self) {
        self.record.exit_entered.store(true, Ordering::SeqCst);
        self.record.changed.notify_waiters();
        let mut released = self.record.release.0.lock().unwrap();
        while !*released {
            released = self.record.release.1.wait(released).unwrap();
        }
        self.record.exited.store(true, Ordering::SeqCst);
        self.record.changed.notify_waiters();
    }
}
pub(super) async fn scope<T>(plan: Arc<FaultPlan>, future: impl Future<Output = T>) -> T {
    CURRENT_PLAN.scope(plan, future).await
}
pub(super) fn claim_worker(profile: CodecProfile) -> Option<WorkerProbe> {
    CURRENT_PLAN
        .try_with(|plan| {
            let mut state = plan.state.lock().unwrap();
            let index = state.workers.len() + 1;
            let record = Arc::new(WorkerRecord {
                profile,
                token: AtomicU64::new(0),
                exit_entered: AtomicBool::new(false),
                exited: AtomicBool::new(false),
                changed: tokio::sync::Notify::new(),
                release: (
                    Mutex::new(state.pause_exit_worker != Some(index)),
                    Condvar::new(),
                ),
                native: Mutex::new(None),
            });
            state.workers.push(record.clone());
            WorkerProbe {
                record,
                fail_at: state
                    .fail_worker
                    .filter(|(worker, _)| *worker == index)
                    .map_or(0, |(_, at)| at),
                close_ready: state.close_ready_worker == Some(index),
            }
        })
        .ok()
}
pub(super) fn witness(probe: &Option<WorkerProbe>) -> Option<Arc<WorkerRecord>> {
    probe.as_ref().map(|probe| probe.record.clone())
}
pub(super) fn bind_worker(record: Option<Arc<WorkerRecord>>, native: &Arc<CodecNative>) {
    if let Some(record) = record {
        *record.native.lock().unwrap() = Some(Arc::downgrade(native));
    }
}
pub(super) fn construct(
    probe: &Option<WorkerProbe>,
    profile: CodecProfile,
) -> std::result::Result<EvsCodec, CodecError> {
    let Some(probe) = probe else {
        return EvsCodec::new(profile);
    };
    let allocation = FaultScope::begin(probe.fail_at).expect("isolated native allocator scope");
    probe
        .record
        .token
        .store(allocation.token(), Ordering::SeqCst);
    let result = EvsCodec::new(profile);
    allocation
        .finish()
        .expect("constructor TLS must end on its actual worker thread");
    result
}
fn evs() -> AudioCodec {
    AudioCodec::Evs { header_full: false }
}
fn wb() -> AudioCodec {
    AudioCodec::AmrWb {
        octet_aligned: true,
        mode: 8,
        policy: sdp::AmrWbModePolicy::new((1 << 2) | (1 << 8), 1, false).unwrap(),
    }
}
fn voiced(frame: usize) -> Vec<u8> {
    (0..320)
        .flat_map(|sample| {
            let t = ((frame * 320 + sample) as f64) / 16000.0;
            ((7000.0 * (std::f64::consts::TAU * 220.0 * t).sin()) as i16).to_le_bytes()
        })
        .collect()
}
async fn measured_attempts(codec: AudioCodec) -> u64 {
    let plan = Arc::new(FaultPlan::default());
    let mut worker = scope(plan.clone(), CodecWorker::new(codec)).await.unwrap();
    worker.close().await.unwrap();
    plan.assert_drained();
    let facts = report(plan.worker(1).token.load(Ordering::SeqCst)).unwrap();
    assert!(facts.attempts > 1 && facts.allocations > 1);
    facts.attempts
}

#[tokio::test]
async fn actual_worker_native_init_failure_and_closed_ready_wait_for_join_then_retry() {
    for codec in [evs(), wb()] {
        let fail_at = measured_attempts(codec).await;
        for closed_ready in [false, true] {
            let plan = Arc::new(FaultPlan::default());
            plan.arm(1, (!closed_ready).then_some(fail_at), closed_ready, true);
            let _release = ReleaseOnDrop(plan.clone());
            let mut creating = Box::pin(scope(plan.clone(), CodecWorker::new(codec)));
            let record = tokio::select! {
                result = &mut creating => panic!("worker returned before actual thread exit gate: {}", result.is_ok()),
                record = plan.wait_worker(1) => record,
            };
            tokio::select! {
                _ = record.wait_exit_gate() => {},
                result = &mut creating => panic!("ready failure escaped join: {}", result.is_ok()),
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut creating)
                    .await
                    .is_err(),
                "ready failure cannot return while its actual thread is held before exit"
            );
            record.release_exit();
            let error = tokio::time::timeout(Duration::from_secs(3), creating)
                .await
                .unwrap()
                .err()
                .expect("constructor must fail");
            if closed_ready {
                assert!(error.uncertain);
            } else {
                assert_eq!(error.code, "ims_media_codec_unavailable");
            }
            let facts = report(record.token.load(Ordering::SeqCst)).unwrap();
            assert_eq!(facts.injected_failures, u64::from(!closed_ready));
            plan.assert_drained();
            let mut retry = scope(plan.clone(), CodecWorker::new(codec)).await.unwrap();
            assert!(!retry.encode(&voiced(1)).await.unwrap().is_empty());
            retry.close().await.unwrap();
            plan.assert_drained();
        }
    }
}

struct NetworkPair {
    owner: Arc<Owner>,
    remote: PacketSocket,
    stop: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    generation: SessionGeneration,
}
// A real registry with optional loss of its retirement receipt. Only the
// transport-failure test disables confirmation; native construction, packet
// handling and socket ownership remain the same actual implementations.
struct TestRegistry {
    registry: UdpSocketRegistry,
    confirm_retirement: bool,
}
impl proletarius_vowifi::netstack_runtime::NetworkRuntime for TestRegistry {
    type ApplicationError = std::convert::Infallible;
    fn service(
        &mut self,
        interface: &mut smoltcp::iface::Interface,
        sockets: &mut SocketSet<'static>,
    ) -> std::result::Result<(), Self::ApplicationError> {
        self.registry.service(interface, sockets);
        Ok(())
    }
    fn retire(&mut self, sockets: &mut SocketSet<'static>) {
        if self.confirm_retirement {
            proletarius_vowifi::netstack_runtime::NetworkRuntime::retire(
                &mut self.registry,
                sockets,
            );
        }
    }
    fn transform_packet(
        &mut self,
        _: proletarius_vowifi::netstack_runtime::PacketDirection,
        packet: Zeroizing<Vec<u8>>,
    ) -> std::io::Result<proletarius_vowifi::netstack_runtime::PacketTransformOutcome> {
        Ok(proletarius_vowifi::netstack_runtime::PacketTransformOutcome::Forward(packet))
    }
}
impl NetworkPair {
    async fn new(plan: Arc<FaultPlan>) -> Self {
        Self::with_retirement(plan, true).await
    }
    async fn with_retirement(plan: Arc<FaultPlan>, confirm_retirement: bool) -> Self {
        let config = |ip: &str| TunnelConfiguration {
            ipv4_addresses: vec![ip.parse().unwrap()],
            ..Default::default()
        };
        let a = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
        let b = TunnelNetwork::new(&config("10.0.0.3")).unwrap();
        let (ab_tx, ab_rx) = mpsc::channel(256);
        let (ba_tx, ba_rx) = mpsc::channel(256);
        let (stop, stop_rx) = watch::channel(false);
        let (wa, ra) = watch::channel(0);
        let (wb, rb) = watch::channel(0);
        let (ca, sa) = UdpSocketRegistry::new(wa);
        let (cb, sb) = UdpSocketRegistry::new(wb);
        let a_stop = stop_rx.clone();
        let tasks = vec![
            tokio::spawn(async move {
                a.run_managed(
                    SocketSet::new(vec![]),
                    ba_rx,
                    ab_tx,
                    a_stop,
                    ra,
                    TestRegistry {
                        registry: sa,
                        confirm_retirement,
                    },
                )
                .await
                .unwrap();
            }),
            tokio::spawn(async move {
                b.run_managed(SocketSet::new(vec![]), ab_rx, ba_tx, stop_rx, rb, sb)
                    .await
                    .unwrap();
            }),
        ];
        let generation = SessionGeneration::from_value(572).unwrap();
        let owner = Arc::new(Owner {
            generation,
            local_ip: "10.0.0.2".parse().unwrap(),
            udp: ca,
            state: tokio::sync::Mutex::new(None),
            native_fault: Some(plan),
        });
        let remote = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
        Self {
            owner,
            remote,
            stop,
            tasks,
            generation,
        }
    }
    fn binding(&self, call: &str) -> CallBinding {
        CallBinding {
            generation: self.generation,
            call_id: call.into(),
            invite_cseq: 1,
        }
    }
    fn negotiation(&self, codec: AudioCodec) -> AudioNegotiation {
        AudioNegotiation {
            remote_ip: self.remote.local_addr().ip(),
            remote_port: self.remote.local_addr().port(),
            codec,
            payload_type: if matches!(codec, AudioCodec::Evs { .. }) {
                96
            } else {
                98
            },
            telephone_event_payload: Some(102),
            telephone_event_mask: 0xffff,
        }
    }
    async fn finish(mut self) {
        assert!(self.owner.state.lock().await.is_none());
        self.remote.close().await.unwrap();
        self.stop.send_replace(true);
        for task in self.tasks {
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap();
        }
    }
}

// A failed assertion must not strand a deliberately paused native thread.
struct ReleaseOnDrop(Arc<FaultPlan>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        for worker in &self.0.state.lock().unwrap().workers {
            worker.release_exit();
        }
    }
}

#[tokio::test]
async fn actual_owner_replacement_native_failures_preserve_old_evs_and_wb_rtp_pcm() {
    for (old_codec, replacement) in [(evs(), wb()), (wb(), evs())] {
        let fail_at = measured_attempts(replacement).await;
        let plan = Arc::new(FaultPlan::default());
        let _release = ReleaseOnDrop(plan.clone());
        let mut network = NetworkPair::new(plan.clone()).await;
        let binding = network.binding("replacement-native-failure");
        let local = network
            .owner
            .clone()
            .allocate(binding.clone())
            .await
            .unwrap();
        let old_remote = network.negotiation(old_codec);
        network
            .owner
            .clone()
            .configure(binding.clone(), old_remote.clone())
            .await
            .unwrap();
        network
            .owner
            .clone()
            .start(binding.clone(), false)
            .await
            .unwrap();
        let first_token = plan.worker(1).token.load(Ordering::SeqCst);
        let mut peer = CodecWorker::new(old_codec).await.unwrap();
        let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bridge = SocketAddr::from(([127, 0, 0, 1], local.audio_port));
        let prefix = format!("{}\0", local.audio_token).into_bytes();
        let mut previous: Option<(u32, u16, u32)> = None;
        let mut frames = 0u64;
        for phase in 0..3 {
            if phase == 1 {
                plan.arm(2, Some(fail_at), false, false);
                let before = network.owner.statistics(&binding);
                let error = network
                    .owner
                    .clone()
                    .prepare(binding.clone(), network.negotiation(replacement))
                    .await
                    .unwrap_err();
                assert_eq!(error.code, "ims_media_codec_unavailable");
                let failed = plan.worker(2);
                assert!(failed.exited.load(Ordering::SeqCst));
                let facts = report(failed.token.load(Ordering::SeqCst)).unwrap();
                assert_eq!(
                    (facts.injected_failures, facts.live_blocks, facts.live_bytes),
                    (1, 0, 0)
                );
                assert_eq!(
                    network.owner.statistics(&binding).rtp_packets_sent,
                    before.rtp_packets_sent
                );
                assert_eq!(
                    network.owner.statistics(&binding).rtp_packets_received,
                    before.rtp_packets_received
                );
                assert_eq!(plan.worker(1).token.load(Ordering::SeqCst), first_token);
                assert!(
                    report(first_token).unwrap().live_blocks > 0,
                    "old actual native codec remains owned"
                );
            } else if phase == 2 {
                network
                    .owner
                    .clone()
                    .prepare(binding.clone(), network.negotiation(replacement))
                    .await
                    .unwrap();
                assert_eq!(
                    plan.len(),
                    3,
                    "failed init must allow a fresh actual constructor retry"
                );
                network
                    .owner
                    .clone()
                    .discard(binding.clone())
                    .await
                    .unwrap();
                let retry = plan.worker(3);
                assert!(retry.exited.load(Ordering::SeqCst));
                assert_eq!(
                    report(retry.token.load(Ordering::SeqCst))
                        .unwrap()
                        .live_blocks,
                    0
                );
            }
            let mut peer_speech = false;
            let mut host_speech = false;
            for frame in 0..4 {
                let mut pcm = prefix.clone();
                pcm.extend(voiced(phase * 4 + frame));
                host.send_to(&pcm, bridge).await.unwrap();
                let packet = tokio::time::timeout(Duration::from_secs(3), network.remote.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(packet.peer, SocketAddr::new(local.ip, local.port));
                let rtp = RtpPacket::parse(&packet.bytes).unwrap();
                assert_eq!(rtp.payload_type, old_remote.payload_type);
                if let Some((ssrc, seq, ts)) = previous {
                    assert_eq!(
                        (rtp.ssrc, rtp.sequence, rtp.timestamp),
                        (ssrc, seq.wrapping_add(1), ts.wrapping_add(320))
                    );
                }
                previous = Some((rtp.ssrc, rtp.sequence, rtp.timestamp));
                peer_speech |= peer
                    .decode(rtp.payload)
                    .await
                    .unwrap()
                    .iter()
                    .any(|byte| *byte != 0);
                network
                    .remote
                    .sender()
                    .send_to(packet.bytes.to_vec(), SocketAddr::new(local.ip, local.port))
                    .await
                    .unwrap();
                let mut received = [0u8; 2048];
                let (len, source) =
                    tokio::time::timeout(Duration::from_secs(3), host.recv_from(&mut received))
                        .await
                        .unwrap()
                        .unwrap();
                assert_eq!(source, bridge);
                assert_eq!(len, prefix.len() + 640);
                assert_eq!(&received[..prefix.len()], prefix.as_slice());
                host_speech |= received[prefix.len()..len].iter().any(|byte| *byte != 0);
                frames += 1;
            }
            assert!(
                peer_speech && host_speech,
                "actual codecs must remain voiced across failure phase {phase}"
            );
            let stats = network.owner.statistics(&binding);
            assert_eq!(
                (
                    stats.rtp_packets_sent,
                    stats.rtp_packets_received,
                    stats.audio_frames_to_host,
                    stats.rtp_decode_errors
                ),
                (frames, frames, frames, 0)
            );
            assert!(stats.failure_code.is_none());
            assert_eq!(
                network
                    .owner
                    .state
                    .lock()
                    .await
                    .as_ref()
                    .unwrap()
                    .binding
                    .call_id,
                binding.call_id
            );
        }
        peer.close().await.unwrap();
        network.owner.clone().stop(binding).await.unwrap();
        plan.assert_drained();
        let reclaimed = UdpSocket::bind(("127.0.0.1", local.audio_port))
            .await
            .expect("same token PCM socket must actually close");
        drop(reclaimed);
        network.finish().await;
    }
}

#[tokio::test]
async fn actual_owner_second_local_offer_worker_failure_joins_first_and_keeps_tx_closed() {
    let plan = Arc::new(FaultPlan::default());
    let _release = ReleaseOnDrop(plan.clone());
    // The WB worker fails inside native initialization after the outer
    // allocation. The first EVS worker is paused only during actual exit.
    plan.arm(2, Some(2), false, false);
    plan.state.lock().unwrap().pause_exit_worker = Some(1);
    let mut network = NetworkPair::new(plan.clone()).await;
    let binding = network.binding("dual-offer-native-failure");
    let local = network
        .owner
        .clone()
        .allocate(binding.clone())
        .await
        .unwrap();
    let offer = sdp::build_local_sdp(
        572,
        local.ip,
        local.port,
        false,
        None,
        None,
        Some(102),
        false,
    )
    .unwrap()
    .into_bytes();
    let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let bridge = SocketAddr::from(([127, 0, 0, 1], local.audio_port));
    let mut pcm = format!("{}\0", local.audio_token).into_bytes();
    pcm.extend(voiced(1));
    host.send_to(&pcm, bridge).await.unwrap();
    let mut preparing = Box::pin(
        network
            .owner
            .clone()
            .prepare_local_offer(binding.clone(), offer.clone()),
    );
    let first = tokio::select! {
        result = &mut preparing => panic!("dual offer returned without draining first native worker: {}", result.is_ok()),
        record = plan.wait_worker(1) => record,
    };
    tokio::select! { _ = first.wait_exit_gate() => {}, result = &mut preparing => panic!("dual offer escaped actual first worker join: {}", result.is_ok()) }
    assert_eq!(plan.len(), 2);
    assert_eq!(first.profile, CodecProfile::EvsPrimaryWb132);
    assert_eq!(plan.worker(2).profile, CodecProfile::AmrWb { mode: 8 });
    assert!(plan.worker(2).exited.load(Ordering::SeqCst));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut preparing)
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), network.remote.recv())
            .await
            .is_err(),
        "even authenticated PCM cannot transmit a partial local offer"
    );
    first.release_exit();
    let error = tokio::time::timeout(Duration::from_secs(3), preparing)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "ims_media_codec_unavailable");
    plan.assert_drained();
    let stats = network.owner.statistics(&binding);
    assert_eq!(
        (
            stats.rtp_packets_sent,
            stats.rtp_packets_received,
            stats.audio_frames_to_host
        ),
        (0, 0, 0)
    );
    assert!(
        network
            .owner
            .clone()
            .dtmf(binding.clone(), '#')
            .await
            .is_err()
    );
    network
        .owner
        .clone()
        .prepare_local_offer(binding.clone(), offer)
        .await
        .unwrap();
    assert_eq!(plan.len(), 4);
    network
        .owner
        .clone()
        .discard(binding.clone())
        .await
        .unwrap();
    plan.assert_drained();
    network.owner.clone().stop(binding).await.unwrap();
    network.finish().await;
}

async fn receive_response(output: &mut mpsc::UnboundedReceiver<Response>) -> Response {
    tokio::time::timeout(Duration::from_secs(3), output.recv())
        .await
        .expect("native fault SIP response deadline")
        .expect("native fault SIP writer closed")
}

fn service_config() -> Config {
    Config {
        local_ip: "10.0.0.2".parse().unwrap(),
        port_c: 5060,
        contact: "<sip:a@10.0.0.2:5060>".into(),
        from_uri: "sip:a@ims.test".into(),
        preferred_identity: "sip:a@ims.test".into(),
        route: "<sip:edge.ims.test;lr>".into(),
        service_routes: vec!["<sip:registered.ims.test;lr>".into()],
        encoded_icsi: "urn%3Aurn-7%3A3gpp-service.ims.icsi.mmtel".into(),
        phone_context: String::new(),
        home_domain: "ims.test".into(),
        realm: "ims.test".into(),
        user_agent: "native-allocation-fault-test".into(),
        security_verify: String::new(),
        pani: String::new(),
        early_media_header: false,
        evs_only: false,
        sms_only: false,
        ussd_service_selector: false,
    }
}
fn offerless_invite(call_id: &str) -> Request {
    let wire = format!(
        "INVITE sip:a@ims.test SIP/2.0\r\nVia: SIP/2.0/TCP 10.0.0.3:5060;branch=z9hG4bK-{call_id}\r\nFrom: <sip:b@ims.test>;tag=carrier-{call_id}\r\nTo: <sip:a@ims.test>\r\nCall-ID: {call_id}\r\nCSeq: 1 INVITE\r\nContact: <sip:b@10.0.0.3:5060>\r\nContent-Length: 0\r\n\r\n"
    );
    let (proletarius_sip::Message::Request(request), consumed) = proletarius_sip::Parser::default()
        .parse(wire.as_bytes(), true)
        .unwrap()
    else {
        panic!("INVITE")
    };
    assert_eq!(consumed, wire.len());
    request
}
#[tokio::test]
async fn actual_owner_dual_native_failure_prevents_initial200_until_first_worker_joined() {
    let plan = Arc::new(FaultPlan::default());
    let _release = ReleaseOnDrop(plan.clone());
    plan.arm(2, Some(2), false, false);
    plan.state.lock().unwrap().pause_exit_worker = Some(1);
    let mut network = NetworkPair::new(plan.clone()).await;
    let (commands, mut input) = mpsc::channel(32);
    let (wire, mut output) = mpsc::unbounded_channel();
    let writer = tokio::spawn(async move {
        while let Some(command) = input.recv().await {
            match command {
                Command::RespondDialog {
                    response, reply, ..
                } => {
                    wire.send(response).unwrap();
                    reply.send(Ok(())).unwrap();
                }
                _ => panic!(
                    "failed initial local offer must never submit a request or hide media behind another SIP owner"
                ),
            }
        }
    });
    let mut service = Service::new(
        network.generation,
        service_config(),
        commands,
        network.owner.clone(),
    )
    .unwrap();
    service
        .request(
            SipConnectionId::Primary,
            offerless_invite("native-failed-offer"),
        )
        .await
        .unwrap();
    assert_eq!(receive_response(&mut output).await.status, 100);
    assert_eq!(receive_response(&mut output).await.status, 180);
    let mut answering = Box::pin(service.answer(network.generation));
    let first = tokio::select! {
        result = &mut answering => panic!("answer returned before actual first codec drained: {}", result.is_ok()),
        record = plan.wait_worker(1) => record,
    };
    tokio::select! { _ = first.wait_exit_gate() => {}, result = &mut answering => panic!("initial answer bypassed native join: {}", result.is_ok()) }
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut answering)
            .await
            .is_err()
    );
    assert!(
        matches!(output.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "no final, especially no initial200, may precede native drain"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), network.remote.recv())
            .await
            .is_err()
    );
    first.release_exit();
    let error = tokio::time::timeout(Duration::from_secs(3), answering)
        .await
        .unwrap()
        .err()
        .expect("native failure must reject answer");
    assert_eq!(error.code, "ims_media_codec_unavailable");
    let final_response = tokio::time::timeout(Duration::from_secs(3), output.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(final_response.status, 488);
    assert!(matches!(
        output.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(service.snapshot.state, "failed");
    assert!(!service.snapshot.media_ready);
    assert_eq!(service.snapshot.audio_port, 0);
    assert!(service.snapshot.audio_token.is_empty());
    assert!(network.owner.state.lock().await.is_none());
    plan.assert_drained();
    // The failed pool did not leave either native worker or allocation fence
    // behind. A distinct dialog can prepare both actual codecs and offer200.
    service
        .request(
            SipConnectionId::Primary,
            offerless_invite("native-retry-offer"),
        )
        .await
        .unwrap();
    assert_eq!(receive_response(&mut output).await.status, 100);
    assert_eq!(receive_response(&mut output).await.status, 180);
    let retry = service.answer(network.generation).await.unwrap();
    assert_eq!(retry.state, "connecting");
    assert!(!retry.media_ready);
    assert_eq!(receive_response(&mut output).await.status, 200);
    assert_eq!(plan.len(), 4);
    let host = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut pcm = format!("{}\0", retry.audio_token).into_bytes();
    pcm.extend(voiced(1));
    host.send_to(&pcm, SocketAddr::from(([127, 0, 0, 1], retry.audio_port)))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(40), network.remote.recv())
            .await
            .is_err(),
        "even the successful retry cannot transmit before SDP answer ACK"
    );
    let binding = service.call.as_ref().unwrap().binding.clone();
    assert_eq!(network.owner.statistics(&binding).rtp_packets_sent, 0);
    service.shutdown(network.generation).await.unwrap();
    drop(service);
    tokio::time::timeout(Duration::from_secs(3), writer)
        .await
        .unwrap()
        .unwrap();
    plan.assert_drained();
    network.finish().await;
}

#[tokio::test]
async fn cancelled_native_prepare_caller_cannot_release_the_call_before_worker_join() {
    let plan = Arc::new(FaultPlan::default());
    let _release = ReleaseOnDrop(plan.clone());
    plan.arm(2, Some(2), false, false);
    plan.state.lock().unwrap().pause_exit_worker = Some(1);
    let network = NetworkPair::new(plan.clone()).await;
    let binding = network.binding("cancelled-native-prepare");
    let local = network
        .owner
        .clone()
        .allocate(binding.clone())
        .await
        .unwrap();
    let offer = sdp::build_local_sdp(
        572,
        local.ip,
        local.port,
        false,
        None,
        None,
        Some(102),
        false,
    )
    .unwrap()
    .into_bytes();
    let mut preparing = Box::pin(
        network
            .owner
            .clone()
            .prepare_local_offer(binding.clone(), offer),
    );
    let first = tokio::select! {
        result = &mut preparing => panic!("partial pool returned before its first worker join: {}", result.is_ok()),
        record = plan.wait_worker(1) => record,
    };
    tokio::select! { _ = first.wait_exit_gate() => {}, result = &mut preparing => panic!("partial pool escaped join: {}", result.is_ok()) }
    drop(preparing);
    // Cancellation drops the caller's wait, not its retained media operation.
    assert!(
        network
            .owner
            .clone()
            .allocate(network.binding("new-call-too-early"))
            .await
            .is_err()
    );
    let mut stopping = Box::pin(network.owner.clone().stop(binding));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut stopping)
            .await
            .is_err(),
        "stop cannot release allocation while native exit is still held"
    );
    assert!(
        UdpSocket::bind(("127.0.0.1", local.audio_port))
            .await
            .is_err(),
        "old PCM port stays owned through native cleanup"
    );
    first.release_exit();
    tokio::time::timeout(Duration::from_secs(3), stopping)
        .await
        .unwrap()
        .unwrap();
    plan.assert_drained();
    let reclaimed = UdpSocket::bind(("127.0.0.1", local.audio_port))
        .await
        .unwrap();
    drop(reclaimed);
    let retry_binding = network.binding("new-call-after-join");
    network
        .owner
        .clone()
        .allocate(retry_binding.clone())
        .await
        .unwrap();
    network
        .owner
        .clone()
        .prepare(retry_binding.clone(), network.negotiation(evs()))
        .await
        .unwrap();
    network.owner.clone().stop(retry_binding).await.unwrap();
    plan.assert_drained();
    network.finish().await;
}

mod runtime_exit_tests;
