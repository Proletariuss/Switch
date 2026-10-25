//! Actual tunneled RTP and token-authenticated Swift PCM bridge.
//! One task owns packet ordering, DTMF and media sockets. Codec state remains
//! on its own retained thread; it is never unsafely marked Send across tasks.
use super::*;
use proletarius_call::{
    RtpPacket, RtpReorder, SequenceDisposition, SequenceTracker, amr_wb,
    evs::{CodecProfile, EvsCodec},
};
use proletarius_vowifi::netstack_udp::{PacketSocket, UdpControl};
use std::{
    net::SocketAddr,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use subtle::ConstantTimeEq;
use tokio::{net::UdpSocket, sync::watch};
use zeroize::Zeroizing;
#[derive(Clone, Copy, Default, Serialize)]
pub(crate) struct Stats {
    pub rtp_packets_sent: u64,
    pub rtp_packets_received: u64,
    pub rtp_decode_errors: u64,
    pub audio_frames_to_host: u64,
    #[serde(skip)]
    pub failure_code: Option<&'static str>,
}
struct Entry {
    binding: CallBinding,
    commands: mpsc::Sender<Control>,
    stop: watch::Sender<bool>,
    done: watch::Receiver<Option<Result<()>>>,
    stats: Arc<Mutex<Stats>>,
}
impl Drop for Entry {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
pub(crate) struct Owner {
    generation: SessionGeneration,
    local_ip: IpAddr,
    udp: UdpControl,
    state: tokio::sync::Mutex<Option<Entry>>,
    #[cfg(all(test, feature = "native-codec-fault-tests"))]
    native_fault: Option<Arc<native_fault_tests::FaultPlan>>,
}
enum Control {
    PrepareLocalOffer(Vec<u8>, oneshot::Sender<Result<()>>),
    Prepare(AudioNegotiation, oneshot::Sender<Result<()>>),
    Configure(AudioNegotiation, oneshot::Sender<Result<()>>),
    Discard(oneshot::Sender<Result<()>>),
    HoldTransmit(bool, oneshot::Sender<Result<()>>),
    Start(bool, oneshot::Sender<Result<()>>),
    Dtmf(char, oneshot::Sender<Result<()>>),
}
impl Owner {
    pub(crate) fn new(
        generation: SessionGeneration,
        local_ip: IpAddr,
        udp: UdpControl,
    ) -> Arc<Self> {
        Arc::new(Self {
            generation,
            local_ip,
            udp,
            state: tokio::sync::Mutex::new(None),
            #[cfg(all(test, feature = "native-codec-fault-tests"))]
            native_fault: None,
        })
    }
    fn check(&self, binding: &CallBinding) -> Result<()> {
        if binding.generation != self.generation {
            Err(Failure::known("ims_media_stale_generation"))
        } else {
            Ok(())
        }
    }
    async fn allocate_inner(self: Arc<Self>, binding: CallBinding) -> Result<LocalMedia> {
        self.check(&binding)?;
        let mut state = self.state.lock().await;
        if state.is_some() {
            return Err(Failure::known("ims_media_busy"));
        }
        let mut rtp = self
            .udp
            .bind(self.local_ip)
            .await
            .map_err(|_| Failure::known("ims_media_tunnel_socket_failed"))?;
        let audio = match UdpSocket::bind("127.0.0.1:0").await {
            Ok(audio) => audio,
            Err(_) => {
                rtp.close().await.map_err(|_| Failure::unknown())?;
                return Err(Failure::known("ims_media_audio_socket_failed"));
            }
        };
        let audio_port = audio
            .local_addr()
            .map_err(|_| Failure::known("ims_media_audio_socket_failed"))?
            .port();
        let token = unique();
        let port = rtp.local_addr().port();
        let (commands, input) = mpsc::channel(16);
        let (stop, stop_rx) = watch::channel(false);
        let (done, done_rx) = watch::channel(None);
        let stats = Arc::new(Mutex::new(Stats::default()));
        *state = Some(Entry {
            binding,
            commands,
            stop,
            done: done_rx,
            stats: stats.clone(),
        });
        let token_for_worker = token.clone();
        #[cfg(all(test, feature = "native-codec-fault-tests"))]
        let native_fault = self.native_fault.clone();
        tokio::spawn(async move {
            let result = run(
                rtp,
                audio,
                Zeroizing::new(token_for_worker),
                input,
                stop_rx,
                stats,
                #[cfg(all(test, feature = "native-codec-fault-tests"))]
                native_fault,
            )
            .await;
            done.send_replace(Some(result));
        });
        Ok(LocalMedia {
            ip: self.local_ip,
            port,
            audio_port,
            audio_token: token,
        })
    }
    async fn control(
        self: Arc<Self>,
        binding: CallBinding,
        make: impl FnOnce(oneshot::Sender<Result<()>>) -> Control,
    ) -> Result<()> {
        self.check(&binding)?;
        let commands = {
            let state = self.state.lock().await;
            let entry = state
                .as_ref()
                .filter(|e| e.binding == binding)
                .ok_or_else(|| Failure::known("ims_media_stale_call"))?;
            if entry.done.borrow().is_some() {
                return Err(Failure::known("ims_media_worker_stopped"));
            }
            entry.commands.clone()
        };
        let (reply, result) = oneshot::channel();
        commands
            .send(make(reply))
            .await
            .map_err(|_| Failure::known("ims_media_worker_stopped"))?;
        result.await.map_err(|_| Failure::unknown())?
    }
    async fn stop_inner(self: Arc<Self>, binding: CallBinding) -> Result<()> {
        self.check(&binding)?;
        let mut done = {
            let state = self.state.lock().await;
            let Some(entry) = state.as_ref() else {
                return Ok(());
            };
            if entry.binding != binding {
                return Err(Failure::known("ims_media_stale_call"));
            }
            entry.stop.send_replace(true);
            entry.done.clone()
        };
        loop {
            if let Some(result) = done.borrow().clone() {
                result?;
                break;
            }
            done.changed().await.map_err(|_| Failure::unknown())?;
        }
        let mut state = self.state.lock().await;
        if state.as_ref().is_some_and(|e| e.binding == binding) {
            state.take();
        }
        Ok(())
    }
}
fn retained<T: Send + 'static>(
    future: impl Future<Output = Result<T>> + Send + 'static,
) -> MediaFuture<T> {
    Box::pin(async move { tokio::spawn(future).await.map_err(|_| Failure::unknown())? })
}
impl Media for Owner {
    fn terminal_failure(&self, binding: &CallBinding) -> Option<Failure> {
        self.statistics(binding).failure_code.map(Failure::known)
    }
    fn statistics(&self, binding: &CallBinding) -> Stats {
        self.state
            .try_lock()
            .ok()
            .and_then(|state| {
                state
                    .as_ref()
                    .filter(|e| &e.binding == binding)
                    .map(|e| *e.stats.lock().unwrap())
            })
            .unwrap_or_default()
    }
    fn allocate(self: Arc<Self>, binding: CallBinding) -> MediaFuture<LocalMedia> {
        retained(self.allocate_inner(binding))
    }
    fn prepare(self: Arc<Self>, binding: CallBinding, remote: AudioNegotiation) -> MediaFuture<()> {
        retained(self.control(binding, move |reply| Control::Prepare(remote, reply)))
    }
    fn prepare_local_offer(
        self: Arc<Self>,
        binding: CallBinding,
        offer: Vec<u8>,
    ) -> MediaFuture<()> {
        retained(self.control(binding, move |reply| {
            Control::PrepareLocalOffer(offer, reply)
        }))
    }
    fn discard(self: Arc<Self>, binding: CallBinding) -> MediaFuture<()> {
        retained(self.control(binding, Control::Discard))
    }
    fn configure(
        self: Arc<Self>,
        binding: CallBinding,
        remote: AudioNegotiation,
    ) -> MediaFuture<()> {
        retained(self.control(binding, move |reply| Control::Configure(remote, reply)))
    }
    fn hold_transmit(self: Arc<Self>, binding: CallBinding, held: bool) -> MediaFuture<()> {
        retained(self.control(binding, move |reply| Control::HoldTransmit(held, reply)))
    }
    fn start(self: Arc<Self>, binding: CallBinding, early: bool) -> MediaFuture<()> {
        retained(self.control(binding, move |reply| Control::Start(early, reply)))
    }
    fn stop(self: Arc<Self>, binding: CallBinding) -> MediaFuture<()> {
        retained(self.stop_inner(binding))
    }
    fn dtmf(self: Arc<Self>, binding: CallBinding, digit: char) -> MediaFuture<()> {
        retained(self.control(binding, move |reply| Control::Dtmf(digit, reply)))
    }
}
enum CodecCommand {
    Encode(
        Box<[i16; 320]>,
        Option<u8>,
        oneshot::Sender<Result<(Vec<u8>, u8)>>,
    ),
    Decode(Vec<u8>, u8, bool, oneshot::Sender<Result<Vec<u8>>>),
    #[cfg(all(test, feature = "native-codec-fault-tests"))]
    TestAction(Box<dyn FnOnce() -> bool + Send>),
}
#[derive(Clone, Copy)]
struct TransmitState {
    mode: u8,
    frames: u64,
}
const CODEC_STOPPED: &str = "ims_media_codec_worker_stopped";
struct MediaHealth {
    failure: watch::Sender<Option<&'static str>>,
}
impl Default for MediaHealth {
    fn default() -> Self {
        Self {
            failure: watch::channel(None).0,
        }
    }
}
impl MediaHealth {
    fn fail(&self) {
        self.failure.send_if_modified(|failure| {
            if failure.is_none() {
                *failure = Some(CODEC_STOPPED);
                true
            } else {
                false
            }
        });
    }
    fn failure(&self) -> Option<Failure> {
        self.failure.borrow().map(Failure::known)
    }
    async fn wait_failure(&self) -> Failure {
        let mut changed = self.failure.subscribe();
        loop {
            if let Some(code) = *changed.borrow() {
                return Failure::known(code);
            }
            if changed.changed().await.is_err() {
                return Failure::known(CODEC_STOPPED);
            }
        }
    }
}
struct CodecHealth {
    media: Arc<MediaHealth>,
    registered: AtomicBool,
    closing: AtomicBool,
    failed: AtomicBool,
}
impl CodecHealth {
    fn register(&self) {
        self.registered.store(true, Ordering::SeqCst);
        if self.failed.load(Ordering::SeqCst) {
            self.media.fail();
        }
    }
    fn failed(&self) {
        self.failed.store(true, Ordering::SeqCst);
        if self.registered.load(Ordering::SeqCst) {
            self.media.fail();
        }
    }
}
struct CodecThreadExit(Arc<CodecHealth>);
impl Drop for CodecThreadExit {
    fn drop(&mut self) {
        if std::thread::panicking() || !self.0.closing.load(Ordering::SeqCst) {
            self.0.failed();
        }
    }
}
struct CodecNative {
    transmit: Mutex<TransmitState>,
    sender: Mutex<Option<std::sync::mpsc::SyncSender<CodecCommand>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    health: Arc<CodecHealth>,
}
impl Drop for CodecNative {
    fn drop(&mut self) {
        // Last-resort cancellation. Normal retirement uses close and joins.
        self.health.closing.store(true, Ordering::SeqCst);
        self.sender.get_mut().unwrap().take();
    }
}
struct CodecWorker {
    native: Option<Arc<CodecNative>>,
    codec: AudioCodec,
    transmit_mode: u8,
    requested_mode: Option<u8>,
    received_cmr: bool,
    encoded_frames: u64,
}
impl CodecWorker {
    async fn new(codec: AudioCodec) -> Result<Self> {
        Self::with_health(codec, Arc::default()).await
    }
    async fn with_health(codec: AudioCodec, media: Arc<MediaHealth>) -> Result<Self> {
        let profile = match codec {
            AudioCodec::Evs { .. } => CodecProfile::EvsPrimaryWb132,
            AudioCodec::AmrWb { mode, policy, .. } if policy.contains(mode) => {
                CodecProfile::AmrWb { mode }
            }
            AudioCodec::AmrWb { .. } => {
                return Err(Failure::known("ims_media_codec_policy_invalid"));
            }
        };
        let (sender, input) = std::sync::mpsc::sync_channel(8);
        let (ready, created) = oneshot::channel();
        let health = Arc::new(CodecHealth {
            media,
            registered: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        });
        let thread_health = health.clone();
        #[cfg(all(test, feature = "native-codec-fault-tests"))]
        let probe = native_fault_tests::claim_worker(profile);
        #[cfg(all(test, feature = "native-codec-fault-tests"))]
        let witness = native_fault_tests::witness(&probe);
        let worker = std::thread::Builder::new()
            .name("native-ims-codec".into())
            .spawn(move || {
                #[cfg(all(test, feature = "native-codec-fault-tests"))]
                let probe = probe;
                // Declared after the test exit gate so health is published
                // before that gate can pause the actual thread's final exit.
                let _exiting = CodecThreadExit(thread_health);
                #[cfg(all(test, feature = "native-codec-fault-tests"))]
                let created = native_fault_tests::construct(&probe, profile);
                #[cfg(not(all(test, feature = "native-codec-fault-tests")))]
                let created = EvsCodec::new(profile);
                #[cfg(all(test, feature = "native-codec-fault-tests"))]
                if probe.as_ref().is_some_and(|probe| probe.close_ready()) {
                    drop(ready);
                    panic!("controlled native codec ready-channel close");
                }
                let mut state = match created {
                    Ok(state) => {
                        let _ = ready.send(Ok(()));
                        state
                    }
                    Err(_) => {
                        let _ = ready.send(Err(Failure::known("ims_media_codec_unavailable")));
                        return;
                    }
                };
                while let Ok(command) = input.recv() {
                    match command {
                        CodecCommand::Encode(mut pcm, mode, reply) => {
                            let result = (|| {
                                if let Some(mode) = mode {
                                    state.set_amr_wb_mode(mode).map_err(|_| {
                                        Failure::known("ims_media_mode_change_failed")
                                    })?;
                                }
                                let bits = state
                                    .encode(&pcm)
                                    .map_err(|_| Failure::known("ims_media_encode_failed"))?;
                                let actual = match state.profile() {
                                    CodecProfile::EvsPrimaryWb132 => 4,
                                    CodecProfile::AmrWb { mode } => mode,
                                };
                                Ok((bits, actual))
                            })();
                            pcm.zeroize();
                            let _ = reply.send(result);
                        }
                        CodecCommand::Decode(mut bits, mode, lost, reply) => {
                            let result = state
                                .decode(&bits, mode, lost)
                                .map(|mut pcm| {
                                    let out = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
                                    pcm.zeroize();
                                    out
                                })
                                .map_err(|_| Failure::known("ims_media_decode_failed"));
                            bits.zeroize();
                            let _ = reply.send(result);
                        }
                        #[cfg(all(test, feature = "native-codec-fault-tests"))]
                        CodecCommand::TestAction(action) => {
                            if !action() {
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(|_| Failure::known("ims_media_codec_worker_failed"))?;
        let initialized = created.await;
        if !matches!(initialized, Ok(Ok(()))) {
            drop(sender);
            tokio::task::spawn_blocking(move || worker.join())
                .await
                .map_err(|_| Failure::unknown())?
                .map_err(|_| Failure::unknown())?;
            return Err(match initialized {
                Ok(Err(error)) => error,
                Err(_) => Failure::unknown(),
                Ok(Ok(())) => unreachable!(),
            });
        }
        health.register();
        let native = Arc::new(CodecNative {
            transmit: Mutex::new(TransmitState {
                mode: match codec {
                    AudioCodec::AmrWb { mode, .. } => mode,
                    AudioCodec::Evs { .. } => 4,
                },
                frames: 0,
            }),
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            health,
        });
        #[cfg(all(test, feature = "native-codec-fault-tests"))]
        native_fault_tests::bind_worker(witness, &native);
        Ok(Self {
            native: Some(native),
            codec,
            transmit_mode: match codec {
                AudioCodec::AmrWb { mode, .. } => mode,
                AudioCodec::Evs { .. } => 4,
            },
            requested_mode: None,
            received_cmr: false,
            encoded_frames: 0,
        })
    }
    fn sync_transmit(&mut self) {
        let transmit = *self.native.as_ref().unwrap().transmit.lock().unwrap();
        self.transmit_mode = transmit.mode;
        self.encoded_frames = transmit.frames;
    }
    async fn encode(&mut self, pcm: &[u8]) -> Result<Vec<u8>> {
        // Receiver retirement order must never select stale encoder metadata.
        // Every alias shares the same native predictor and authoritative TX
        // state; each alias's codec and CMR still describe its own wire policy.
        self.sync_transmit();
        let mut samples = Box::new([0; 320]);
        for (out, pair) in samples.iter_mut().zip(pcm.chunks_exact(2)) {
            *out = i16::from_le_bytes([pair[0], pair[1]]);
        }
        let (reply, result) = oneshot::channel();
        let next_mode = match self.codec {
            AudioCodec::AmrWb { policy, mode, .. } => {
                // A newly accepted set overrides period/neighbor for the first
                // frame if the former mode is no longer legal (RFC4867 8.2).
                Some(if policy.contains(self.transmit_mode) {
                    policy.next_mode(self.transmit_mode, self.requested_mode, self.encoded_frames)
                } else {
                    mode
                })
            }
            AudioCodec::Evs { .. } => None,
        };
        self.native
            .as_ref()
            .unwrap()
            .sender
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .try_send(CodecCommand::Encode(
                samples,
                next_mode.filter(|mode| *mode != self.transmit_mode),
                reply,
            ))
            .map_err(codec_send_failure)?;
        let (bits, actual_mode) = self.reply(result).await?;
        self.transmit_mode = actual_mode;
        self.encoded_frames = self.encoded_frames.wrapping_add(1);
        *self.native.as_ref().unwrap().transmit.lock().unwrap() = TransmitState {
            mode: actual_mode,
            frames: self.encoded_frames,
        };
        match self.codec {
            AudioCodec::Evs { header_full } => {
                proletarius_call::evs::packetize_primary(&bits, 4, header_full)
                    .map_err(|_| Failure::known("ims_media_packetize_failed"))
            }
            AudioCodec::AmrWb { octet_aligned, .. } => {
                amr_wb::packetize(&bits, actual_mode, octet_aligned)
                    .map_err(|_| Failure::known("ims_media_packetize_failed"))
            }
        }
    }
    fn frame(
        codec: AudioCodec,
        payload: &[u8],
    ) -> Result<(
        Vec<u8>,
        u8,
        bool,
        Option<(u8, proletarius_call::sdp::AmrWbModePolicy)>,
    )> {
        Ok(match codec {
            AudioCodec::Evs { header_full } => {
                let frame = proletarius_call::evs::depacketize_primary(payload, header_full)
                    .map_err(|_| Failure::known("ims_media_payload_invalid"))?;
                if !matches!(frame.mode, 4 | 12 | 14 | 15) {
                    return Err(Failure::known("ims_media_payload_invalid"));
                }
                (frame.bits.to_vec(), frame.mode, frame.mode == 14, None)
            }
            AudioCodec::AmrWb {
                octet_aligned,
                policy,
                ..
            } => {
                let frame = amr_wb::depacketize(payload, octet_aligned)
                    .map_err(|_| Failure::known("ims_media_payload_invalid"))?;
                if frame.mode <= 8 && !policy.contains(frame.mode) {
                    return Err(Failure::known("ims_media_payload_invalid"));
                }
                let lost = !frame.good || frame.mode == 14;
                (
                    if lost { vec![] } else { frame.bits },
                    frame.mode,
                    lost,
                    Some((frame.raw_cmr, policy)),
                )
            }
        })
    }
    async fn decode(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let (bits, mode, lost, cmr) = Self::frame(self.codec, payload)?;
        let pcm = self.decode_bits(bits, mode, lost).await?;
        if let Some((cmr, policy)) = cmr {
            match cmr {
                0..=8 if policy.contains(cmr) => {
                    self.requested_mode = Some(cmr);
                    self.received_cmr = true;
                }
                15 => {
                    self.requested_mode = None;
                    self.received_cmr = true;
                }
                _ => {} // Reserved or outside the negotiated set: ignore.
            }
        }
        Ok(pcm)
    }
    async fn decode_bits(&self, bits: Vec<u8>, mode: u8, lost: bool) -> Result<Vec<u8>> {
        let (reply, result) = oneshot::channel();
        self.native
            .as_ref()
            .unwrap()
            .sender
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .try_send(CodecCommand::Decode(bits, mode, lost, reply))
            .map_err(codec_send_failure)?;
        self.reply(result).await
    }
    async fn reply<T>(&self, result: oneshot::Receiver<Result<T>>) -> Result<T> {
        tokio::select! {
            biased;
            failure = self.native.as_ref().unwrap().health.media.wait_failure() => Err(failure),
            result = result => result.map_err(|_| Failure::known(CODEC_STOPPED))?,
        }
    }
    fn fork(&self, codec: AudioCodec) -> Self {
        let requested_mode = match codec {
            AudioCodec::AmrWb { policy, .. } => {
                self.requested_mode.filter(|mode| policy.contains(*mode))
            }
            _ => None,
        };
        let transmit = *self.native.as_ref().unwrap().transmit.lock().unwrap();
        Self {
            native: self.native.clone(),
            codec,
            transmit_mode: transmit.mode,
            requested_mode,
            received_cmr: false,
            encoded_frames: transmit.frames,
        }
    }
    async fn close(&mut self) -> Result<()> {
        let Some(native) = self.native.take() else {
            return Ok(());
        };
        // Other receive formats can share this native predictor. Its final
        // context owns the join, after all senders have been released.
        if let Ok(native) = Arc::try_unwrap(native) {
            native.health.closing.store(true, Ordering::SeqCst);
            native.sender.lock().unwrap().take();
            let worker = native.worker.lock().unwrap().take();
            if let Some(worker) = worker {
                let joined = tokio::task::spawn_blocking(move || worker.join())
                    .await
                    .map_err(|_| Failure::unknown())?;
                if joined.is_err() {
                    // A panic is a runtime failure, but join has confirmed
                    // this thread's exit. Do not turn it into an eternal
                    // unconfirmed-cleanup fence after every socket closes.
                    native.health.failed();
                }
            }
        }
        Ok(())
    }
}
fn codec_send_failure(error: std::sync::mpsc::TrySendError<CodecCommand>) -> Failure {
    match error {
        std::sync::mpsc::TrySendError::Full(_) => Failure::known("ims_media_codec_busy"),
        std::sync::mpsc::TrySendError::Disconnected(_) => Failure::known(CODEC_STOPPED),
    }
}
fn same_family(a: AudioCodec, b: AudioCodec) -> bool {
    matches!(
        (a, b),
        (AudioCodec::Evs { .. }, AudioCodec::Evs { .. })
            | (AudioCodec::AmrWb { .. }, AudioCodec::AmrWb { .. })
    )
}
fn same_wire(a: AudioCodec, b: AudioCodec) -> bool {
    match (a, b) {
        // hf-only restricts the permitted EVS payload forms. With hf-only=0,
        // the same parser already accepts both compact and full-header frames;
        // changing that restriction does not rebind the PT to a new codec.
        (AudioCodec::Evs { .. }, AudioCodec::Evs { .. }) => true,
        (
            AudioCodec::AmrWb {
                octet_aligned: a, ..
            },
            AudioCodec::AmrWb {
                octet_aligned: b, ..
            },
        ) => a == b,
        _ => false,
    }
}
#[derive(Default)]
struct EndpointIngress {
    reorder: RtpReorder,
    receiver_ids: std::collections::HashMap<u16, u64>,
    ssrc: Option<u32>,
}
impl EndpointIngress {
    fn push(&mut self, receiver_id: u64, packet: &RtpPacket<'_>, now: std::time::Instant) {
        if self.ssrc != Some(packet.ssrc) {
            self.ssrc = Some(packet.ssrc);
            self.receiver_ids.clear();
        }
        if self.reorder.push(packet, now) {
            self.receiver_ids.insert(packet.sequence, receiver_id);
        }
    }
    fn pop(&mut self, now: std::time::Instant) -> Option<(u64, proletarius_call::OwnedRtpPacket)> {
        let packet = self.reorder.pop(now)?;
        let receiver_id = self
            .receiver_ids
            .remove(&packet.sequence)
            .expect("each accepted RTP packet retains its receiver identity");
        Some((receiver_id, packet))
    }
    fn contains_receiver(&self, receiver_id: u64) -> bool {
        self.receiver_ids.values().any(|id| *id == receiver_id)
    }
}
struct Receiver {
    id: u64,
    remote: AudioNegotiation,
    codec: CodecWorker,
    ingress: Arc<Mutex<EndpointIngress>>,
    sequence: Arc<Mutex<SequenceTracker>>,
}
impl Receiver {
    fn endpoint(&self) -> SocketAddr {
        SocketAddr::new(self.remote.remote_ip, self.remote.remote_port)
    }
    fn accepts(&self, source: SocketAddr, packet: &RtpPacket<'_>) -> bool {
        self.endpoint() == source
            && self.remote.payload_type == packet.payload_type
            && CodecWorker::frame(self.remote.codec, packet.payload).is_ok()
    }
}
const RECEIVE_TRANSITION: Duration = Duration::from_secs(60);
const MAX_RETIRED_RECEIVERS: usize = 8;
const MAX_PREANSWER_PACKETS: usize = 32;
const MAX_PREHOST_PCM_FRAMES: usize = 32;
struct LocalReceiveFormat {
    payload_type: u8,
    codec: CodecWorker,
}
struct PreanswerPacket {
    source: SocketAddr,
    packet: proletarius_call::OwnedRtpPacket,
}
/// An unanswered local offer has receive formats, but no negotiated remote
/// endpoint. Never invent one or feed unauthenticated sources into a native
/// decoder. Retain a bounded pre-answer window until the answer selects it.
struct LocalOfferPreparation {
    offer: Zeroizing<Vec<u8>>,
    formats: Vec<LocalReceiveFormat>,
    packets: std::collections::VecDeque<PreanswerPacket>,
}
impl LocalOfferPreparation {
    async fn close(&mut self) -> Result<()> {
        self.packets.clear();
        let mut failure = None;
        for mut format in self.formats.drain(..) {
            if let Err(error) = format.codec.close().await {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    fn buffer(&mut self, source: SocketAddr, packet: &RtpPacket<'_>) {
        if packet.payload.len() > 2048
            || !self.formats.iter().any(|format| {
                format.payload_type == packet.payload_type
                    && CodecWorker::frame(format.codec.codec, packet.payload).is_ok()
            })
        {
            return;
        }
        if self.packets.len() == MAX_PREANSWER_PACKETS {
            self.packets.pop_front();
        }
        self.packets.push_back(PreanswerPacket {
            source,
            packet: packet.into(),
        });
    }
}
fn local_receive_formats(offer: &[u8]) -> Result<Vec<(u8, AudioCodec)>> {
    let invalid = || Failure::known("ims_media_local_offer_invalid");
    if offer.len() > 64 * 1024 {
        return Err(invalid());
    }
    let text = std::str::from_utf8(offer).map_err(|_| invalid())?;
    let mut history = sdp::PayloadHistory::default();
    history.admit_sdp(text).map_err(|_| invalid())?;
    let lines = text.lines().collect::<Vec<_>>();
    let audio = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("m=audio "))
        .collect::<Vec<_>>();
    if audio.len() != 1 {
        return Err(invalid());
    }
    let (audio_index, audio_line) = audio[0];
    let fields = audio_line.split_ascii_whitespace().collect::<Vec<_>>();
    if fields.len() < 4 || fields.len() > 35 {
        return Err(invalid());
    }
    let (evs_available, modes) = proletarius_call::evs::available_audio_codecs(false);
    let mut formats = vec![];
    for payload in &fields[3..] {
        let payload_type = payload.parse::<u8>().map_err(|_| invalid())?;
        if formats.iter().any(|(pt, _)| *pt == payload_type) {
            return Err(invalid());
        }
        let event_mapping = format!("a=rtpmap:{payload} telephone-event/16000");
        if lines.iter().any(|line| {
            line.eq_ignore_ascii_case(&event_mapping)
                || line.eq_ignore_ascii_case(&format!("{event_mapping}/1"))
        }) {
            continue;
        }
        let single = lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                if index == audio_index {
                    format!("{} {} {} {}", fields[0], fields[1], fields[2], payload)
                } else {
                    (*line).to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\r\n");
        let selected = sdp::negotiate_audio_offer_with_modes(&single, evs_available, &modes)
            .map_err(|_| invalid())?;
        if selected.payload_type != payload_type {
            return Err(invalid());
        }
        // The parser's endpoint belongs to our local SDP. Only the format
        // enters this preparation; a remote endpoint first comes from ACK.
        formats.push((payload_type, selected.codec));
    }
    if formats.is_empty() {
        return Err(invalid());
    }
    Ok(formats)
}
#[derive(Default)]
struct NegotiationState {
    health: Arc<MediaHealth>,
    #[cfg(all(test, feature = "native-codec-fault-tests"))]
    native_fault: Option<Arc<native_fault_tests::FaultPlan>>,
    local_offer: Option<LocalOfferPreparation>,
    current: Option<Receiver>,
    pending: Option<Receiver>,
    retired: Vec<(Receiver, Instant)>,
    payload_history: Vec<(u8, AudioCodec)>,
    next_receiver_id: u64,
    // Decoding/CMR cannot wait for Swift to attach after ACK. Keep only this
    // allocation's latest 640ms, zeroizing evicted and stopped-call audio.
    host_pcm: std::collections::VecDeque<Zeroizing<Vec<u8>>>,
}
impl NegotiationState {
    async fn new_worker(&self, codec: AudioCodec) -> Result<CodecWorker> {
        #[cfg(all(test, feature = "native-codec-fault-tests"))]
        if let Some(plan) = &self.native_fault {
            return native_fault_tests::scope(
                plan.clone(),
                CodecWorker::with_health(codec, self.health.clone()),
            )
            .await;
        }
        CodecWorker::with_health(codec, self.health.clone()).await
    }
    async fn prepare_local_offer(&mut self, offer: Vec<u8>) -> Result<()> {
        if self.current.is_some() || self.pending.is_some() || !self.retired.is_empty() {
            return Err(Failure::known("ims_media_negotiation_pending"));
        }
        if let Some(prepared) = &self.local_offer {
            return if prepared.offer.as_slice() == offer.as_slice() {
                Ok(())
            } else {
                Err(Failure::known("ims_media_negotiation_pending"))
            };
        }
        let formats = local_receive_formats(&offer)?;
        let mut prepared = LocalOfferPreparation {
            offer: Zeroizing::new(offer),
            formats: vec![],
            packets: Default::default(),
        };
        for (payload_type, codec) in formats {
            let existing = prepared
                .formats
                .iter()
                .find(|format| same_family(format.codec.codec, codec));
            let worker = match existing {
                Some(format) => Ok(format.codec.fork(codec)),
                None => self.new_worker(codec).await,
            };
            let worker = match worker {
                Ok(worker) => worker,
                Err(error) => {
                    prepared.close().await?;
                    return Err(error);
                }
            };
            prepared.formats.push(LocalReceiveFormat {
                payload_type,
                codec: worker,
            });
        }
        self.local_offer = Some(prepared);
        Ok(())
    }
    async fn prepare(&mut self, candidate: AudioNegotiation) -> Result<()> {
        if let Some(pending) = &self.pending {
            return if pending.remote == candidate {
                Ok(())
            } else {
                Err(Failure::known("ims_media_negotiation_pending"))
            };
        }
        if self.current.as_ref().is_some_and(|r| r.remote == candidate) {
            return Ok(());
        }
        if candidate.payload_type > 127
            || candidate.remote_port == 0
            || candidate
                .telephone_event_payload
                .is_some_and(|pt| pt > 127 || pt == candidate.payload_type)
        {
            return Err(Failure::known("ims_media_payload_invalid"));
        }
        if let AudioCodec::AmrWb { mode, policy, .. } = candidate.codec {
            if !policy.contains(mode) {
                return Err(Failure::known("ims_media_codec_policy_invalid"));
            }
        }
        if self
            .payload_history
            .iter()
            .any(|(pt, codec)| *pt == candidate.payload_type && !same_wire(*codec, candidate.codec))
        {
            return Err(Failure::known("ims_media_payload_rebound"));
        }
        if self.retired.len() >= MAX_RETIRED_RECEIVERS {
            return Err(Failure::known("ims_media_transition_busy"));
        }
        let existing = self
            .current
            .iter()
            .chain(self.retired.iter().rev().map(|(r, _)| r))
            .find(|r| same_family(r.remote.codec, candidate.codec));
        let local_format = self.local_offer.as_ref().and_then(|prepared| {
            prepared.formats.iter().find(|format| {
                format.payload_type == candidate.payload_type
                    && same_wire(format.codec.codec, candidate.codec)
            })
        });
        if self.local_offer.is_some() && local_format.is_none() {
            return Err(Failure::known("ims_media_local_offer_mismatch"));
        }
        let mut codec = match existing {
            Some(r) => r.codec.fork(candidate.codec),
            None => match local_format {
                Some(format) => format.codec.fork(candidate.codec),
                None => self.new_worker(candidate.codec).await?,
            },
        };
        if self
            .current
            .as_ref()
            .is_some_and(|r| !same_family(r.remote.codec, candidate.codec))
        {
            // Reuse predictor and frame phase, but an earlier codec interval's
            // CMR is not a request for this new offer/answer exchange.
            codec.requested_mode = None;
        }
        let endpoint = SocketAddr::new(candidate.remote_ip, candidate.remote_port);
        let previous = self
            .current
            .iter()
            .chain(self.retired.iter().map(|(r, _)| r))
            .find(|r| r.endpoint() == endpoint);
        let sequence = previous
            .map(|r| r.sequence.clone())
            .unwrap_or_else(|| Arc::new(Mutex::new(SequenceTracker::default())));
        let ingress = previous
            .map(|r| r.ingress.clone())
            .unwrap_or_else(|| Arc::new(Mutex::new(EndpointIngress::default())));
        let id = self.next_receiver_id;
        self.next_receiver_id = self
            .next_receiver_id
            .checked_add(1)
            .ok_or_else(|| Failure::known("ims_media_receiver_exhausted"))?;
        self.pending = Some(Receiver {
            id,
            remote: candidate,
            codec,
            ingress,
            sequence,
        });
        if let Some(mut prepared) = self.local_offer.take() {
            let pending = self.pending.as_mut().unwrap();
            for buffered in prepared.packets.drain(..) {
                let packet = RtpPacket {
                    marker: buffered.packet.marker,
                    payload_type: buffered.packet.payload_type,
                    sequence: buffered.packet.sequence,
                    timestamp: buffered.packet.timestamp,
                    ssrc: buffered.packet.ssrc,
                    payload: &buffered.packet.payload,
                };
                if pending.accepts(buffered.source, &packet) {
                    pending.ingress.lock().unwrap().push(
                        pending.id,
                        &packet,
                        std::time::Instant::now(),
                    );
                }
            }
            prepared.close().await?;
        }
        Ok(())
    }
    async fn configure(&mut self, candidate: AudioNegotiation) -> Result<bool> {
        self.prepare(candidate.clone()).await?;
        if self.current.as_ref().is_some_and(|r| r.remote == candidate) {
            return Ok(false);
        }
        let mut pending = self
            .pending
            .take()
            .ok_or_else(|| Failure::known("ims_media_unconfigured"))?;
        if pending.remote != candidate {
            self.pending = Some(pending);
            return Err(Failure::known("ims_media_negotiation_pending"));
        }
        pending.codec.sync_transmit();
        if let Some(current) = &mut self.current {
            if same_family(current.remote.codec, candidate.codec) {
                // A request decoded under the prepared new format takes
                // precedence; otherwise inherit only an authorized old CMR.
                let request = if pending.codec.received_cmr {
                    pending.codec.requested_mode
                } else {
                    current.codec.requested_mode
                };
                pending.codec.requested_mode = match candidate.codec {
                    AudioCodec::AmrWb { policy, .. } => request.filter(|m| policy.contains(*m)),
                    _ => None,
                };
            }
            // The endpoint queue retains immutable receiver IDs. Queued old
            // packets keep their original parser even when the PT is shared.
        }
        if let Some(current) = self.current.take() {
            self.retired
                .push((current, Instant::now() + RECEIVE_TRANSITION));
        }
        if !self
            .payload_history
            .iter()
            .any(|(pt, _)| *pt == candidate.payload_type)
        {
            self.payload_history
                .push((candidate.payload_type, candidate.codec));
        }
        self.current = Some(pending);
        Ok(true)
    }
    async fn discard(&mut self) -> Result<()> {
        let mut failure = None;
        if let Some(mut prepared) = self.local_offer.take() {
            if let Err(error) = prepared.close().await {
                failure = Some(error);
            }
        }
        if let Some(mut pending) = self.pending.take() {
            if let Err(error) = pending.codec.close().await {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    async fn expire(&mut self) -> Result<()> {
        let mut index = 0;
        while index < self.retired.len() {
            if self.retired[index].1 <= Instant::now()
                && !self.retired[index]
                    .0
                    .ingress
                    .lock()
                    .unwrap()
                    .contains_receiver(self.retired[index].0.id)
            {
                let (mut receiver, _) = self.retired.remove(index);
                receiver.codec.close().await?;
            } else {
                index += 1;
            }
        }
        Ok(())
    }
    async fn close(&mut self) -> Result<()> {
        self.host_pcm.clear();
        let mut failure = None;
        if let Err(error) = self.discard().await {
            failure = Some(error);
        }
        if let Some(mut current) = self.current.take() {
            if let Err(error) = current.codec.close().await {
                failure = Some(error);
            }
        }
        for (mut receiver, _) in self.retired.drain(..) {
            if let Err(error) = receiver.codec.close().await {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
struct Dtmf {
    packets: Vec<Vec<u8>>,
    index: usize,
    next: Instant,
    next_timestamp: u32,
    reply: oneshot::Sender<Result<()>>,
}
impl Dtmf {
    fn resume_timestamp(&self) -> u32 {
        // Every scheduled packet occupies 20ms at the 16kHz RTP clock,
        // including the repeated end packets. Unsent intervals do not elapse
        // when a negotiation cancels the remainder of the event.
        self.next_timestamp
            .wrapping_sub((self.packets.len() - self.index) as u32 * 320)
    }
}
async fn stopping(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow() || stop.changed().await.is_err() {
            return;
        }
    }
}
async fn run(
    mut rtp: PacketSocket,
    audio: UdpSocket,
    token: Zeroizing<String>,
    mut controls: mpsc::Receiver<Control>,
    mut stop: watch::Receiver<bool>,
    stats: Arc<Mutex<Stats>>,
    #[cfg(all(test, feature = "native-codec-fault-tests"))] native_fault: Option<
        Arc<native_fault_tests::FaultPlan>,
    >,
) -> Result<()> {
    let mut media = NegotiationState::default();
    let health = media.health.clone();
    #[cfg(all(test, feature = "native-codec-fault-tests"))]
    {
        media.native_fault = native_fault;
    }
    let mut active = false;
    let mut early = false;
    let mut transmit_held = false;
    let mut peer = None;
    let mut dtmf: Option<Dtmf> = None;
    let mut seed = [0; 10];
    getrandom::fill(&mut seed).map_err(|_| Failure::known("ims_media_entropy_failed"))?;
    let mut seq = u16::from_be_bytes(seed[..2].try_into().unwrap());
    let mut timestamp = u32::from_be_bytes(seed[2..6].try_into().unwrap());
    let ssrc = u32::from_be_bytes(seed[6..].try_into().unwrap());
    let mut input = Zeroizing::new(vec![0; 2048]);
    let prefix = Zeroizing::new(format!("{}\0", token.as_str()).into_bytes());
    let result: Result<()> = async {
        loop {
            if let Some(error) = health.failure() { return Err(error); }
            media.expire().await?;
            let reorder_deadline=media.current.iter().chain(media.pending.iter())
                .chain(media.retired.iter().map(|(r,_)|r))
                .filter_map(|r|r.ingress.lock().unwrap().reorder.next_deadline()).min()
                .unwrap_or(std::time::Instant::now()+Duration::from_secs(3600));
            let retirement_deadline=media.retired.iter().map(|(_,deadline)|*deadline)
                .filter(|deadline|*deadline>Instant::now()).min()
                .unwrap_or(Instant::now()+Duration::from_secs(3600));
            let dtmf_deadline=dtmf.as_ref().map(|d|d.next)
                .unwrap_or(Instant::now()+Duration::from_secs(3600));
            tokio::select! { biased;
                _=stopping(&mut stop)=>break,
                error=health.wait_failure()=>return Err(error),
                control=controls.recv()=>{
                    let Some(control)=control else { break; };
                    match control {
                        Control::PrepareLocalOffer(offer,reply)=>{
                            let _=reply.send(media.prepare_local_offer(offer).await);
                        }
                        Control::Prepare(candidate,reply)=>{
                            let _=reply.send(media.prepare(candidate).await);
                        }
                        Control::Discard(reply)=>{let _=reply.send(media.discard().await);}
                        Control::HoldTransmit(held,reply)=>{
                            transmit_held=held;
                            if held {
                                if let Some(job)=dtmf.take() {
                                    timestamp=job.resume_timestamp();
                                    let _=job.reply.send(Err(Failure::known("ims_media_negotiating")));
                                }
                            }
                            let _=reply.send(Ok(()));
                        }
                        Control::Configure(candidate,reply)=>{
                            let result=media.configure(candidate).await;
                            if matches!(result,Ok(true)) {
                                if let Some(job)=dtmf.take() {
                                    timestamp=job.resume_timestamp();
                                    let _=job.reply.send(Err(Failure::known("ims_media_reconfigured")));
                                }
                            }
                            let _=reply.send(result.map(|_|()));
                        }
                        Control::Start(is_early,reply)=>{
                            let result=if media.current.is_some() {
                                active=true;early=is_early;Ok(())
                            } else {Err(Failure::known("ims_media_unconfigured"))};
                            let _=reply.send(result);
                        }
                        Control::Dtmf(digit,reply)=>{
                            if transmit_held {
                                let _=reply.send(Err(Failure::known("ims_media_negotiating")));continue;
                            }
                            let payload=media.current.as_ref().and_then(|r|r.remote.telephone_event_payload);
                            let event=match proletarius_call::dtmf_event(digit) {
                                Ok(event)=>event,
                                Err(_)=>{let _=reply.send(Err(Failure::known("ims_media_dtmf_invalid")));continue;}
                            };
                            let supported=media.current.as_ref().is_some_and(|r|
                                r.remote.telephone_event_mask & (1u16<<event) != 0);
                            if !active||early||payload.is_none()||dtmf.is_some()||!supported {
                                let _=reply.send(Err(Failure::known("ims_media_dtmf_unavailable")));continue;
                            }
                            match proletarius_call::dtmf_rtp_schedule(event,2,payload.unwrap(),seq,timestamp,ssrc) {
                                Ok((packets,_,next_timestamp))=>dtmf=Some(Dtmf {
                                    packets,index:0,next:Instant::now(),next_timestamp,reply
                                }),
                                Err(_)=>{let _=reply.send(Err(Failure::known("ims_media_dtmf_invalid")));}
                            }
                        }
                    }
                }
                _=tokio::time::sleep_until(dtmf_deadline),if dtmf.is_some()=>{
                    let job=dtmf.as_mut().unwrap();
                    let receiver=media.current.as_ref().unwrap();
                    let packet=job.packets[job.index].clone();seq=seq.wrapping_add(1);
                    if rtp.sender().send_to(packet,receiver.endpoint()).await.is_err() {
                        let job=dtmf.take().unwrap();timestamp=job.resume_timestamp();
                        let _=job.reply.send(Err(Failure::unknown()));continue;
                    }
                    stats.lock().unwrap().rtp_packets_sent+=1;job.index+=1;
                    if job.index==job.packets.len() {
                        timestamp=job.next_timestamp;
                        let job=dtmf.take().unwrap();let _=job.reply.send(Ok(()));
                    } else {job.next=Instant::now()+Duration::from_millis(20);}
                }
                result=audio.recv_from(&mut input)=>{
                    let (count,source)=result.map_err(|_|Failure::known("ims_media_audio_read_failed"))?;
                    if !source.ip().is_loopback()||count<prefix.len()||
                        !bool::from(input[..prefix.len()].ct_eq(prefix.as_slice())) {continue;}
                    peer=Some(source);
                    flush_host_pcm(&mut media.host_pcm,&audio,&prefix,source,&stats).await;
                    if count==prefix.len()||!active||early||transmit_held||dtmf.is_some()||count-prefix.len()!=640 {continue;}
                    let receiver=media.current.as_mut().unwrap();
                    let payload=match receiver.codec.encode(&input[prefix.len()..count]).await {
                        Ok(payload)=>payload,
                        Err(error) if error.code=="ims_media_codec_busy"=>continue,
                        Err(error)=>return Err(error),
                    };
                    let packet=rtp_packet(receiver.remote.payload_type,seq,timestamp,ssrc,&payload);
                    seq=seq.wrapping_add(1);timestamp=timestamp.wrapping_add(320);
                    rtp.sender().send_to(packet,receiver.endpoint()).await.map_err(|_|Failure::unknown())?;
                    stats.lock().unwrap().rtp_packets_sent+=1;
                }
                packet=rtp.recv()=>{
                    let packet=packet.map_err(|_|Failure::known("ims_media_tunnel_read_failed"))?;
                    // Initial prepare establishes receive readiness before a
                    // successful SDP answer. TX remains gated independently by
                    // start/early/hold until signaling permits it.
                    if let Ok(parsed)=RtpPacket::parse(&packet.bytes) {
                        if let Some(prepared)=&mut media.local_offer {
                            prepared.buffer(packet.peer,&parsed);
                            continue;
                        }
                        if media.current.is_none() && media.pending.is_none() {continue;}
                        // Every queued packet belongs to a fixed wire format.
                        // Policy-only changes can fall back to the former set,
                        // but framing/family changes require distinct PTs.
                        let mut receivers=media.current.iter_mut().chain(media.pending.iter_mut())
                            .chain(media.retired.iter_mut().rev()
                                .filter(|(_,deadline)|*deadline>Instant::now()).map(|(r,_)|r));
                        if let Some(receiver)=receivers.find(|r|r.accepts(packet.peer,&parsed)) {
                            receiver.ingress.lock().unwrap().push(receiver.id,&parsed,std::time::Instant::now());
                        } else if media.current.as_ref().is_some_and(|r|
                            r.endpoint()==packet.peer && r.remote.payload_type==parsed.payload_type) {
                            // Keep malformed active payloads observable; they
                            // cannot inject a CMR or enter a different decoder.
                            stats.lock().unwrap().rtp_decode_errors+=1;
                        }
                    }
                }
                _=tokio::time::sleep_until(tokio::time::Instant::from_std(reorder_deadline))=>{}
                _=tokio::time::sleep_until(retirement_deadline)=>{}
            }
            if let Some(error) = health.failure() { return Err(error); }
            receive_ready(&mut media,&audio,&prefix,peer,&stats).await?;
        }
        Ok(())
    }.await;
    controls.close();
    if let Some(job) = dtmf.take() {
        let _ = job.reply.send(Err(Failure::known("ims_media_stopped")));
    }
    while let Some(control) = controls.recv().await {
        let reply = match control {
            Control::PrepareLocalOffer(_, reply)
            | Control::Prepare(_, reply)
            | Control::Configure(_, reply)
            | Control::Discard(reply)
            | Control::HoldTransmit(_, reply)
            | Control::Start(_, reply)
            | Control::Dtmf(_, reply) => reply,
        };
        let _ = reply.send(Err(Failure::known("ims_media_stopped")));
    }
    if let Err(error) = result {
        stats.lock().unwrap().failure_code.get_or_insert(error.code);
    }
    let codec_cleanup = media.close().await;
    let udp_cleanup = rtp.close().await.map_err(|_| Failure::unknown());
    drop(audio);
    if let Some(error) = health.failure() {
        stats.lock().unwrap().failure_code.get_or_insert(error.code);
    }
    codec_cleanup?;
    udp_cleanup
}
async fn receive_ready(
    media: &mut NegotiationState,
    audio: &UdpSocket,
    prefix: &[u8],
    peer: Option<SocketAddr>,
    stats: &Mutex<Stats>,
) -> Result<()> {
    if let Some(peer) = peer {
        flush_host_pcm(&mut media.host_pcm, audio, prefix, peer, stats).await;
    }
    let mut ingresses: Vec<Arc<Mutex<EndpointIngress>>> = vec![];
    for receiver in media
        .current
        .iter()
        .chain(media.pending.iter())
        .chain(media.retired.iter().map(|(r, _)| r))
    {
        if !ingresses
            .iter()
            .any(|ingress| Arc::ptr_eq(ingress, &receiver.ingress))
        {
            ingresses.push(receiver.ingress.clone());
        }
    }
    for ingress in ingresses {
        loop {
            let next = ingress.lock().unwrap().pop(std::time::Instant::now());
            let Some((receiver_id, packet)) = next else {
                break;
            };
            let receiver = media
                .current
                .iter_mut()
                .chain(media.pending.iter_mut())
                .chain(media.retired.iter_mut().map(|(r, _)| r))
                .find(|r| r.id == receiver_id);
            // Discarded preparations cannot route their queued payload into the
            // active decoder; advancing the endpoint queue releases them.
            let Some(receiver) = receiver else {
                continue;
            };
            let disposition = receiver
                .sequence
                .lock()
                .unwrap()
                .observe(packet.ssrc, packet.sequence);
            if disposition == SequenceDisposition::DuplicateOrLate {
                continue;
            }
            stats.lock().unwrap().rtp_packets_received += 1;
            if let SequenceDisposition::Missing(count) = disposition {
                for _ in 0..count {
                    match receiver.codec.decode_bits(vec![], 14, true).await {
                        Ok(pcm) => {
                            queue_host_pcm(&mut media.host_pcm, pcm);
                            if let Some(peer) = peer {
                                flush_host_pcm(&mut media.host_pcm, audio, prefix, peer, stats)
                                    .await;
                            }
                        }
                        Err(error) if error.code == CODEC_STOPPED => return Err(error),
                        Err(_) => {}
                    }
                }
            }
            let pcm = match receiver.codec.decode(&packet.payload).await {
                Ok(pcm) => pcm,
                Err(error) => {
                    if error.code == CODEC_STOPPED {
                        return Err(error);
                    }
                    stats.lock().unwrap().rtp_decode_errors += 1;
                    match receiver.codec.decode_bits(vec![], 14, true).await {
                        Ok(pcm) => pcm,
                        Err(error) if error.code == CODEC_STOPPED => return Err(error),
                        Err(_) => continue,
                    }
                }
            };
            queue_host_pcm(&mut media.host_pcm, pcm);
            if let Some(peer) = peer {
                flush_host_pcm(&mut media.host_pcm, audio, prefix, peer, stats).await;
            }
        }
    }
    Ok(())
}

fn rtp_packet(
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + payload.len());
    out.extend_from_slice(&[0x80, payload_type & 0x7f]);
    out.extend_from_slice(&sequence.to_be_bytes());
    out.extend_from_slice(&timestamp.to_be_bytes());
    out.extend_from_slice(&ssrc.to_be_bytes());
    out.extend_from_slice(payload);
    out
}
async fn deliver(
    audio: &UdpSocket,
    prefix: &[u8],
    pcm: Zeroizing<Vec<u8>>,
    peer: SocketAddr,
    stats: &Mutex<Stats>,
) {
    if pcm.len() != 640 {
        return;
    }
    let mut out = Zeroizing::new(Vec::with_capacity(prefix.len() + 640));
    out.extend_from_slice(prefix);
    out.extend_from_slice(&pcm);
    if audio.send_to(&out, peer).await.is_ok() {
        stats.lock().unwrap().audio_frames_to_host += 1;
    }
}
fn queue_host_pcm(queue: &mut std::collections::VecDeque<Zeroizing<Vec<u8>>>, pcm: Vec<u8>) {
    let pcm = Zeroizing::new(pcm);
    if pcm.len() != 640 {
        return;
    }
    if queue.len() == MAX_PREHOST_PCM_FRAMES {
        queue.pop_front();
    }
    queue.push_back(pcm);
}
async fn flush_host_pcm(
    queue: &mut std::collections::VecDeque<Zeroizing<Vec<u8>>>,
    audio: &UdpSocket,
    prefix: &[u8],
    peer: SocketAddr,
    stats: &Mutex<Stats>,
) {
    while let Some(pcm) = queue.pop_front() {
        deliver(audio, prefix, pcm, peer, stats).await;
    }
}

#[cfg(all(test, feature = "native-codec-fault-tests"))]
mod native_fault_tests;
#[cfg(test)]
mod tests;
