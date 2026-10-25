//! Registered IMS owner, gated until native SMS/call handlers are complete.
//! Commands/events are bounded and tagged with one reader/SIM generation.
#![allow(
    dead_code,
    reason = "native production owner remains gated pending SMS/call handlers"
)]

use crate::native_vowifi_supervisor::{NativeSessionPermit, ReaderSessionKey, SessionGeneration};
use proletarius_sip::{Request, Response};
use proletarius_vowifi::{
    registration_coordinator::{CleanupReport, SipConnectionId},
    sip_session_runtime::{
        RegisteredSipEvent, RegisteredSipMessage, RegisteredSipRuntime, ResponseTicket,
    },
};
use std::{future::Future, io, time::Duration};
use tokio::sync::{mpsc, oneshot, watch};

const CAPACITY: usize = 32;
const TERMINAL_DRAIN: Duration = Duration::from_secs(5);

/// Owns cancellation, exposes no abort operation, and lets resource cleanup
/// keep running even if the caller abandons the completion future.
pub(crate) struct Task<T> {
    completion: Option<tokio::task::JoinHandle<T>>,
    stop: watch::Sender<bool>,
    finished: bool,
}

impl<T: Send + 'static> Task<T> {
    pub(crate) fn spawn(
        stop: watch::Sender<bool>,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Self {
        Self {
            completion: Some(tokio::spawn(future)),
            stop,
            finished: false,
        }
    }

    pub(crate) fn request_stop(&self) {
        self.stop.send_replace(true);
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.completion
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
    }

    /// Poll only a completed owner without blocking a service-state lock. An
    /// unfinished task keeps its original join handle and cancellation owner.
    pub(crate) fn try_finish(&mut self) -> Option<Result<T, tokio::task::JoinError>> {
        let handle = self.completion.as_mut()?;
        if !handle.is_finished() {
            return None;
        }
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match std::pin::Pin::new(handle).poll(&mut context) {
            std::task::Poll::Pending => None,
            std::task::Poll::Ready(result) => {
                self.completion.take();
                self.finished = true;
                Some(result)
            }
        }
    }

    pub(crate) async fn finish(mut self) -> Result<T, tokio::task::JoinError> {
        let result = self
            .completion
            .take()
            .expect("completion consumed once")
            .await;
        self.finished = result.is_ok();
        result
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if !self.finished {
            self.stop.send_replace(true);
        }
        // Dropping JoinHandle detaches the owner; it does not abort cleanup.
    }
}

pub(crate) enum Command {
    SendUntracked {
        generation: SessionGeneration,
        request: Request,
        reply: oneshot::Sender<io::Result<()>>,
    },
    RespondDialog {
        generation: SessionGeneration,
        connection: SipConnectionId,
        response: Response,
        reply: oneshot::Sender<io::Result<()>>,
    },
    Send {
        generation: SessionGeneration,
        request: Request,
        timeout: Duration,
        reply: oneshot::Sender<io::Result<ResponseTicket>>,
    },
    Respond {
        generation: SessionGeneration,
        connection: SipConnectionId,
        request: Box<RegisteredSipMessage>,
        status: u16,
        reason: String,
        reply: oneshot::Sender<io::Result<()>>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IncomingReply {
    Unanswered,
    Sent,
    WriteUnknown,
}

pub(crate) struct Event {
    pub(crate) reply_state: IncomingReply,
    pub(crate) generation: SessionGeneration,
    pub(crate) event: RegisteredSipEvent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopReason {
    Requested,
    LeaseLost,
    ConsumerGone,
    InputClosed,
    StreamClosed,
    StreamFailed,
    HeartbeatFailed,
    WrongSession,
}

pub(crate) struct Exit {
    pub(crate) permit: NativeSessionPermit,
    pub(crate) reason: StopReason,
    pub(crate) cleanup: Result<(), CleanupReport>,
    /// Exact generation-owned events that could not be delivered before the
    /// bounded terminal drain. Payloads remain private and never enter Debug.
    pub(crate) undelivered: Vec<Event>,
}

pub(crate) fn channels() -> (
    mpsc::Sender<Command>,
    mpsc::Receiver<Command>,
    mpsc::Sender<Event>,
    mpsc::Receiver<Event>,
) {
    let (commands, input) = mpsc::channel(CAPACITY);
    let (events, output) = mpsc::channel(CAPACITY);
    (commands, input, events, output)
}

pub(crate) trait SessionDriver: Send {
    fn heartbeat_deadline(&self) -> Option<tokio::time::Instant> {
        None
    }
    fn heartbeat_tick(&mut self) -> impl Future<Output = io::Result<()>> + Send {
        async { Ok(()) }
    }
    fn send_untracked(&mut self, _: Request) -> impl Future<Output = io::Result<()>> + Send {
        async {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "dialog transport unavailable",
            ))
        }
    }
    fn respond_dialog(
        &mut self,
        _: SipConnectionId,
        _: Response,
    ) -> impl Future<Output = io::Result<()>> + Send {
        async {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "dialog transport unavailable",
            ))
        }
    }
    fn next_event(&mut self)
    -> impl Future<Output = io::Result<Option<RegisteredSipEvent>>> + Send;
    fn send(
        &mut self,
        request: Request,
        timeout: Duration,
    ) -> impl Future<Output = io::Result<ResponseTicket>> + Send;
    fn respond(
        &mut self,
        connection: SipConnectionId,
        request: &RegisteredSipMessage,
        status: u16,
        reason: &str,
    ) -> impl Future<Output = io::Result<()>> + Send;
    fn shutdown(self) -> impl Future<Output = Result<(), CleanupReport>> + Send;
}

impl SessionDriver for RegisteredSipRuntime {
    fn heartbeat_deadline(&self) -> Option<tokio::time::Instant> {
        self.heartbeat_deadline()
    }
    async fn heartbeat_tick(&mut self) -> io::Result<()> {
        self.heartbeat_tick().await
    }
    async fn send_untracked(&mut self, request: Request) -> io::Result<()> {
        self.send_dialog_request(request).await
    }
    async fn respond_dialog(
        &mut self,
        connection: SipConnectionId,
        response: Response,
    ) -> io::Result<()> {
        self.send_dialog_response(connection, response).await
    }
    async fn next_event(&mut self) -> io::Result<Option<RegisteredSipEvent>> {
        self.next_event().await
    }
    async fn send(&mut self, request: Request, timeout: Duration) -> io::Result<ResponseTicket> {
        self.send_tracked_request(request, timeout).await
    }
    async fn respond(
        &mut self,
        connection: SipConnectionId,
        request: &RegisteredSipMessage,
        status: u16,
        reason: &str,
    ) -> io::Result<()> {
        self.respond_to(connection, request, status, reason, None)
            .await
    }
    async fn shutdown(self) -> Result<(), CleanupReport> {
        self.shutdown().await
    }
}

fn signal_reason(
    stop: &watch::Receiver<bool>,
    lease: &watch::Receiver<bool>,
) -> Option<StopReason> {
    if *stop.borrow() || stop.has_changed().is_err() {
        Some(StopReason::Requested)
    } else if *lease.borrow() || lease.has_changed().is_err() {
        Some(StopReason::LeaseLost)
    } else {
        None
    }
}

fn stopped_error() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "IMS session stopped")
}
fn stale_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "stale IMS session generation")
}

/// The spawned owner must await this completion before signalling IKE stop.
/// A bounded event consumer handles durable SMS commit and call policy; this
/// loop never sends an automatic successful MESSAGE acknowledgement.
pub(crate) async fn run<D: SessionDriver>(
    mut driver: D,
    expected_key: &ReaderSessionKey,
    permit: NativeSessionPermit,
    mut lease_lost: watch::Receiver<bool>,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
) -> Exit {
    let mut stop = permit.stop_signal();
    let generation = permit.generation();
    let mut deferred = std::collections::VecDeque::with_capacity(CAPACITY);
    let mut terminal = None;
    let mut terminal_deadline = None;
    let reason = if !permit.matches_key(expected_key) {
        StopReason::WrongSession
    } else {
        loop {
            if let Some(reason) = signal_reason(&stop, &lease_lost) {
                break reason;
            }
            if deferred.is_empty() {
                if let Some(reason) = terminal {
                    break reason;
                }
            }
            let terminal_wait = async {
                if let Some(deadline) = terminal_deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            let can_read = terminal.is_none() && deferred.len() < CAPACITY;
            let heartbeat_deadline = if can_read || terminal.is_some() {
                None
            } else {
                driver.heartbeat_deadline()
            };
            let heartbeat_wait = async {
                if let Some(deadline) = heartbeat_deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! {
                _=stop.changed()=>{continue;}
                _=lease_lost.changed()=>{continue;}
                _=events.closed()=>{break StopReason::ConsumerGone;}
                _=terminal_wait=>{break terminal.expect("terminal drain has a cause");}
                // Deferred business events remain ordered while the same sole
                // reader can correlate heartbeat responses behind them. There
                // is no lossy broadcast or second socket-reader task.
                slot=events.reserve(),if !deferred.is_empty()=>{
                    let Ok(slot)=slot else {break StopReason::ConsumerGone;};
                    if let Some(reason)=signal_reason(&stop,&lease_lost) {break reason;}
                    slot.send(deferred.pop_front().expect("pending IMS event"));
                }
                _=heartbeat_wait=>{
                    if let Some(reason)=signal_reason(&stop,&lease_lost) {break reason;}
                    if let Err(error)=driver.heartbeat_tick().await {break stream_failure(&error);}
                }
                command=commands.recv()=>{
                    let Some(command)=command else {break StopReason::InputClosed;};
                    if let Some(reason)=signal_reason(&stop,&lease_lost) {reject(command,stopped_error());break reason;}
                    if terminal.is_some() {reject(command,stopped_error());} else {execute(&mut driver,generation,command).await;}
                }
                event=driver.next_event(),if can_read=>{
                    let event=match event {Ok(Some(event))=>event,Ok(None)=>{terminal=Some(StopReason::StreamClosed);terminal_deadline=Some(tokio::time::Instant::now()+TERMINAL_DRAIN);continue;},Err(error)=>{terminal=Some(stream_failure(&error));terminal_deadline=Some(tokio::time::Instant::now()+TERMINAL_DRAIN);continue;}};
                    if let Some(reason)=signal_reason(&stop,&lease_lost) {break reason;}
                    let reply_state=match &event {
                        RegisteredSipEvent::Message {connection,message,..}
                            if matches!(message.as_ref(),RegisteredSipMessage::Request{method,..} if method.as_str()=="OPTIONS")=>{
                            match driver.respond(*connection,message,200,"OK").await {Ok(())=>IncomingReply::Sent,Err(_)=>IncomingReply::WriteUnknown}
                        }
                        _=>IncomingReply::Unanswered,
                    };
                    deferred.push_back(Event{generation,event,reply_state});
                }
            }
        }
    };
    commands.close();
    while let Some(command) = commands.recv().await {
        reject(command, stopped_error());
    }
    let cleanup = driver.shutdown().await;
    Exit {
        permit,
        reason,
        cleanup,
        undelivered: deferred.into_iter().collect(),
    }
}

fn stream_failure(error: &io::Error) -> StopReason {
    if error.get_ref().is_some_and(|source| {
        source.is::<proletarius_vowifi::sip_session_runtime::HeartbeatFailure>()
    }) {
        StopReason::HeartbeatFailed
    } else {
        StopReason::StreamFailed
    }
}

async fn execute<D: SessionDriver>(
    driver: &mut D,
    generation: SessionGeneration,
    command: Command,
) {
    match command {
        Command::SendUntracked {
            generation: requested,
            request,
            reply,
        } => {
            let result = if requested != generation {
                Err(stale_error())
            } else {
                driver.send_untracked(request).await
            };
            let _ = reply.send(result);
        }
        Command::RespondDialog {
            generation: requested,
            connection,
            response,
            reply,
        } => {
            let result = if requested != generation {
                Err(stale_error())
            } else {
                driver.respond_dialog(connection, response).await
            };
            let _ = reply.send(result);
        }
        Command::Send {
            generation: requested,
            request,
            timeout,
            reply,
        } => {
            if requested != generation {
                let _ = reply.send(Err(stale_error()));
            } else if !reply.is_closed() {
                let _ = reply.send(driver.send(request, timeout).await);
            }
        }
        Command::Respond {
            generation: requested,
            connection,
            request,
            status,
            reason,
            reply,
        } => {
            if requested != generation {
                let _ = reply.send(Err(stale_error()));
            } else if !reply.is_closed() {
                let _ = reply.send(driver.respond(connection, &request, status, &reason).await);
            }
        }
    }
}

fn reject(command: Command, error: io::Error) {
    match command {
        Command::SendUntracked { reply, .. } | Command::RespondDialog { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        Command::Send { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        Command::Respond { reply, .. } => {
            let _ = reply.send(Err(error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_vowifi_supervisor::{CompletionDisposition, NativeVoWiFiSupervisor};
    use proletarius_sip::{Message, Parser};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct FixtureDriver {
        incoming: mpsc::Receiver<RegisteredSipEvent>,
        writes: Arc<AtomicUsize>,
        reads: Arc<AtomicUsize>,
        cleaned: Arc<AtomicUsize>,
        response_failure: bool,
        response_connections: Arc<std::sync::Mutex<Vec<SipConnectionId>>>,
        event_read: Option<oneshot::Sender<()>>,
        cleanup_started: Option<oneshot::Sender<()>>,
        cleanup_release: Option<oneshot::Receiver<()>>,
    }
    impl SessionDriver for FixtureDriver {
        async fn send_untracked(&mut self, _: Request) -> io::Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn respond_dialog(
            &mut self,
            connection: SipConnectionId,
            _: Response,
        ) -> io::Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.response_connections.lock().unwrap().push(connection);
            Ok(())
        }

        async fn next_event(&mut self) -> io::Result<Option<RegisteredSipEvent>> {
            let event = self.incoming.recv().await;
            if event.is_some() {
                self.reads.fetch_add(1, Ordering::SeqCst);
            }
            if let Some(read) = self.event_read.take() {
                let _ = read.send(());
            }
            Ok(event)
        }
        async fn send(&mut self, _: Request, _: Duration) -> io::Result<ResponseTicket> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "synthetic send result",
            ))
        }
        async fn respond(
            &mut self,
            connection: SipConnectionId,
            _: &RegisteredSipMessage,
            _: u16,
            _: &str,
        ) -> io::Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.response_connections.lock().unwrap().push(connection);
            if self.response_failure {
                return Err(io::Error::other("synthetic response uncertainty"));
            }
            Ok(())
        }
        async fn shutdown(self) -> Result<(), CleanupReport> {
            if let Some(started) = self.cleanup_started {
                let _ = started.send(());
            }
            if let Some(release) = self.cleanup_release {
                let _ = release.await;
            }
            self.cleaned.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    fn key() -> ReaderSessionKey {
        ReaderSessionKey::new("fixture-reader", "8901234567890123456").unwrap()
    }
    fn fixture() -> (
        FixtureDriver,
        mpsc::Sender<RegisteredSipEvent>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let (tx, incoming) = mpsc::channel(64);
        let writes = Arc::new(AtomicUsize::new(0));
        let cleaned = Arc::new(AtomicUsize::new(0));
        (
            FixtureDriver {
                incoming,
                writes: writes.clone(),
                reads: Arc::new(AtomicUsize::new(0)),
                cleaned: cleaned.clone(),
                response_failure: false,
                response_connections: Arc::new(std::sync::Mutex::new(Vec::new())),
                event_read: None,
                cleanup_started: None,
                cleanup_release: None,
            },
            tx,
            writes,
            cleaned,
        )
    }
    fn request() -> Request {
        let wire = b"OPTIONS sip:ims.example SIP/2.0\r\nVia: SIP/2.0/TCP fixture;branch=z9hG4bKtest\r\nFrom: <sip:a@ims.example>;tag=a\r\nTo: <sip:b@ims.example>\r\nCall-ID: fixture\r\nCSeq: 1 OPTIONS\r\nContent-Length: 0\r\n\r\n";
        let (Message::Request(request), _) = Parser::default().parse(wire, false).unwrap() else {
            panic!("fixture")
        };
        request
    }

    #[tokio::test]
    async fn dialog_commands_fence_generation_and_retain_accepted_write_after_receipt_drop() {
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor.reserve(key()).unwrap();
        let generation = permit.generation();
        let stale = SessionGeneration::from_value(generation.value() + 1).unwrap();
        let (mut driver, _, writes, _) = fixture();
        let (reply, receipt) = oneshot::channel();
        execute(
            &mut driver,
            generation,
            Command::SendUntracked {
                generation: stale,
                request: request(),
                reply,
            },
        )
        .await;
        assert_eq!(
            receipt.await.unwrap().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        let (reply, receipt) = oneshot::channel();
        drop(receipt);
        execute(
            &mut driver,
            generation,
            Command::SendUntracked {
                generation,
                request: request(),
                reply,
            },
        )
        .await;
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "accepted dialog write survives abandoned response view"
        );
        let response = Response {
            status: 200,
            reason: "OK".into(),
            data: request().data,
        };
        let (reply, receipt) = oneshot::channel();
        execute(
            &mut driver,
            generation,
            Command::RespondDialog {
                generation: stale,
                connection: SipConnectionId::Primary,
                response: response.clone(),
                reply,
            },
        )
        .await;
        assert!(receipt.await.unwrap().is_err());
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        let (reply, receipt) = oneshot::channel();
        drop(receipt);
        execute(
            &mut driver,
            generation,
            Command::RespondDialog {
                generation,
                connection: SipConnectionId::Primary,
                response,
                reply,
            },
        )
        .await;
        assert_eq!(writes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn options_is_replied_once_and_forwarded_with_reply_state() {
        for failed in [false, true] {
            let mut supervisor = NativeVoWiFiSupervisor::default();
            let permit = supervisor.reserve(key()).unwrap();
            let (mut driver, incoming, writes, _) = fixture();
            driver.response_failure = failed;
            let observed_connections = driver.response_connections.clone();
            let (commands, input, events, mut output) = channels();
            let (_lease, lease_rx) = watch::channel(false);
            let cancel = permit.cancellation_sender();
            let task =
                tokio::spawn(
                    async move { run(driver, &key(), permit, lease_rx, input, events).await },
                );
            let req = request();
            incoming
                .send(RegisteredSipEvent::Message {
                    connection: SipConnectionId::Primary,
                    message: Box::new(RegisteredSipMessage::Request {
                        method: req.method,
                        uri: req.uri,
                        version: req.data.version,
                        headers: req.data.headers,
                        body: zeroize::Zeroizing::new(req.data.body),
                    }),
                    response_ticket: None,
                })
                .await
                .unwrap();
            let event = output.recv().await.unwrap();
            assert_eq!(
                event.reply_state,
                if failed {
                    IncomingReply::WriteUnknown
                } else {
                    IncomingReply::Sent
                }
            );
            assert_eq!(
                *observed_connections.lock().unwrap(),
                vec![SipConnectionId::Primary]
            );
            assert_eq!(writes.load(Ordering::SeqCst), 1);
            assert!(matches!(event.event, RegisteredSipEvent::Message { .. }));
            incoming
                .send(RegisteredSipEvent::KeepAlive {
                    connection: SipConnectionId::Primary,
                })
                .await
                .unwrap();
            assert_eq!(
                output.recv().await.unwrap().reply_state,
                IncomingReply::Unanswered
            );
            assert_eq!(writes.load(Ordering::SeqCst), 1);
            cancel.send_replace(true);
            assert_eq!(task.await.unwrap().reason, StopReason::Requested);
            drop(commands);
        }
    }

    #[tokio::test]
    async fn abandoning_task_waiter_requests_stop_but_background_cleanup_releases_slot() {
        for abandon_waiter in [false, true] {
            let supervisor = Arc::new(std::sync::Mutex::new(NativeVoWiFiSupervisor::default()));
            let permit = supervisor.lock().unwrap().reserve(key()).unwrap();
            supervisor.lock().unwrap().mark_running(&permit).unwrap();
            let cancel = permit.cancellation_sender();
            let mut stop = permit.stop_signal();
            let weak = Arc::downgrade(&supervisor);
            let (started, started_rx) = oneshot::channel();
            let (release, release_rx) = oneshot::channel();
            let (done, done_rx) = oneshot::channel();
            let task = Task::spawn(cancel, async move {
                if !*stop.borrow() {
                    stop.changed().await.unwrap();
                }
                assert!(*stop.borrow());
                started.send(()).unwrap();
                release_rx.await.unwrap();
                let owner = weak.upgrade().unwrap();
                assert_eq!(
                    owner.lock().unwrap().worker_finished(&permit),
                    CompletionDisposition::Removed
                );
                done.send(()).unwrap();
            });
            if abandon_waiter {
                let waiter = tokio::spawn(async move { task.finish().await });
                tokio::task::yield_now().await;
                waiter.abort();
                assert!(waiter.await.unwrap_err().is_cancelled());
            } else {
                drop(task);
            }
            tokio::time::timeout(Duration::from_secs(1), started_rx)
                .await
                .unwrap()
                .unwrap();
            assert!(supervisor.lock().unwrap().reserve(key()).is_err());
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(1), done_rx)
                .await
                .unwrap()
                .unwrap();
            assert!(supervisor.lock().unwrap().reserve(key()).is_ok());
        }
    }

    #[test]
    fn dropping_supervisor_signals_stop_even_when_task_retains_cancellation_sender() {
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor.reserve(key()).unwrap();
        let retained_sender = permit.cancellation_sender();
        let stop = permit.stop_signal();
        drop(supervisor);
        assert!(*stop.borrow());
        assert!(retained_sender.receiver_count() > 0);
    }

    #[test]
    fn old_task_cancellation_never_stops_replacement_generation() {
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let old = supervisor.reserve(key()).unwrap();
        let cancel = old.cancellation_sender();
        assert_eq!(
            supervisor.worker_finished(&old),
            CompletionDisposition::Removed
        );
        let replacement = supervisor.reserve(key()).unwrap();
        cancel.send_replace(true);
        assert!(!*replacement.stop_signal().borrow());
        assert_eq!(
            supervisor.snapshot(&key()).unwrap().generation,
            replacement.generation()
        );
    }

    #[tokio::test]
    async fn rejects_stale_commands_then_cleans_up_before_releasing_supervisor_slot() {
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor.reserve(key()).unwrap();
        supervisor.mark_running(&permit).unwrap();
        let wrong = supervisor
            .reserve(ReaderSessionKey::new("other", "8901234567890123456").unwrap())
            .unwrap();
        let stop = permit.stop_signal();
        let (mut driver, _incoming, writes, cleaned) = fixture();
        let (started, started_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        driver.cleanup_started = Some(started);
        driver.cleanup_release = Some(release_rx);
        let (commands, input, events, _output) = channels();
        let (_lease, lease) = watch::channel(false);
        let gen_id = permit.generation();
        let task =
            tokio::spawn(async move { run(driver, &key(), permit, lease, input, events).await });
        let (reply, result) = oneshot::channel();
        commands
            .send(Command::Send {
                generation: wrong.generation(),
                request: request(),
                timeout: Duration::from_secs(1),
                reply,
            })
            .await
            .unwrap();
        assert_eq!(
            result.await.unwrap().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        // Find this exact reserved generation without exposing its key/id.
        assert_eq!(supervisor.snapshot(&key()).unwrap().generation, gen_id);
        supervisor.request_stop_all();
        started_rx.await.unwrap();
        assert!(*stop.borrow());
        assert!(supervisor.reserve(key()).is_err());
        assert!(!task.is_finished());
        release.send(()).unwrap();
        let exit = task.await.unwrap();
        assert_eq!(exit.reason, StopReason::Requested);
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
        assert_eq!(
            supervisor.worker_finished(&exit.permit),
            CompletionDisposition::Removed
        );
        let replacement = supervisor.reserve(key()).unwrap();
        assert_ne!(replacement.generation(), gen_id);
        assert_eq!(
            supervisor.worker_finished(&exit.permit),
            CompletionDisposition::StaleGeneration
        );
    }

    #[tokio::test]
    async fn lease_loss_interrupts_full_event_queue_and_drains_queued_commands() {
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor.reserve(key()).unwrap();
        let gen_id = permit.generation();
        let (mut driver, incoming, writes, cleaned) = fixture();
        let (read, read_rx) = oneshot::channel();
        driver.event_read = Some(read);
        let (commands, input, events, mut output) = channels();
        for _ in 0..CAPACITY {
            events
                .send(Event {
                    reply_state: IncomingReply::Unanswered,
                    generation: gen_id,
                    event: RegisteredSipEvent::KeepAlive {
                        connection: SipConnectionId::Primary,
                    },
                })
                .await
                .unwrap();
        }
        incoming
            .send(RegisteredSipEvent::KeepAlive {
                connection: SipConnectionId::Primary,
            })
            .await
            .unwrap();
        let (lease_tx, lease) = watch::channel(false);
        let task =
            tokio::spawn(async move { run(driver, &key(), permit, lease, input, events).await });
        read_rx.await.unwrap();
        let (reply, result) = oneshot::channel();
        commands
            .send(Command::Send {
                generation: gen_id,
                request: request(),
                timeout: Duration::from_secs(1),
                reply,
            })
            .await
            .unwrap();
        lease_tx.send_replace(true);
        let exit = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exit.reason, StopReason::LeaseLost);
        assert!(result.await.unwrap().is_err());
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
        assert_eq!(output.recv().await.unwrap().generation, gen_id);
    }

    #[tokio::test]
    async fn full_event_queue_still_services_consumer_commands() {
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor.reserve(key()).unwrap();
        let generation = permit.generation();
        let (mut driver, incoming, writes, cleaned) = fixture();
        let (read, read_rx) = oneshot::channel();
        driver.event_read = Some(read);
        let (commands, input, events, _output) = channels();
        for _ in 0..CAPACITY {
            events
                .send(Event {
                    reply_state: IncomingReply::Unanswered,
                    generation,
                    event: RegisteredSipEvent::KeepAlive {
                        connection: SipConnectionId::Primary,
                    },
                })
                .await
                .unwrap();
        }
        incoming
            .send(RegisteredSipEvent::KeepAlive {
                connection: SipConnectionId::Primary,
            })
            .await
            .unwrap();
        let (_lease, lease) = watch::channel(false);
        let task =
            tokio::spawn(async move { run(driver, &key(), permit, lease, input, events).await });
        read_rx.await.unwrap();
        let (reply, result) = oneshot::channel();
        commands
            .send(Command::Send {
                generation,
                request: request(),
                timeout: Duration::from_secs(1),
                reply,
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), result)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        supervisor.request_stop_all();
        let exit = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exit.reason, StopReason::Requested);
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn wrong_reader_key_cannot_execute_io() {
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor.reserve(key()).unwrap();
        let (driver, _incoming, writes, cleaned) = fixture();
        let (_commands, input, events, _output) = channels();
        let (_lease, lease) = watch::channel(false);
        let other = ReaderSessionKey::new("wrong-reader", "8901234567890123456").unwrap();
        let exit = run(driver, &other, permit, lease, input, events).await;
        assert_eq!(exit.reason, StopReason::WrongSession);
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dropped_consumer_and_lost_watch_sender_both_cleanup_without_io() {
        for consumer_gone in [true, false] {
            let mut supervisor = NativeVoWiFiSupervisor::default();
            let permit = supervisor.reserve(key()).unwrap();
            let (driver, _incoming, writes, cleaned) = fixture();
            let (_commands, input, events, output) = channels();
            let (lease_tx, lease) = watch::channel(false);
            if consumer_gone {
                drop(output);
            } else {
                drop(lease_tx);
            }
            let exit = run(driver, &key(), permit, lease, input, events).await;
            assert_eq!(
                exit.reason,
                if consumer_gone {
                    StopReason::ConsumerGone
                } else {
                    StopReason::LeaseLost
                }
            );
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            assert_eq!(cleaned.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn task_owner_can_reap_finished_worker_without_aborting_it() {
        let (stop, mut stopped) = watch::channel(false);
        let task = Task::spawn(stop, async move { stopped.changed().await.is_ok() });
        assert!(!task.is_finished());
        task.request_stop();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(task.finish().await.unwrap());
    }
    #[tokio::test]
    async fn backpressure_preserves_fifo_and_terminal_eof_while_same_reader_observes_health() {
        let (driver, incoming, _writes, cleaned) = fixture();
        let reads = driver.reads.clone();
        let (commands, input, events, mut output) = channels();
        let key =
            ReaderSessionKey::new("reader-backpressure-health", "8944330000000000001").unwrap();
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor.reserve(key.clone()).unwrap();
        let generation = permit.generation();
        let (_lease, lost) = watch::channel(false);
        for _ in 0..CAPACITY {
            events
                .send(Event {
                    generation,
                    reply_state: IncomingReply::Unanswered,
                    event: RegisteredSipEvent::KeepAlive {
                        connection: SipConnectionId::Primary,
                    },
                })
                .await
                .unwrap();
        }
        incoming
            .send(RegisteredSipEvent::HeartbeatHealthChanged { healthy: false })
            .await
            .unwrap();
        incoming
            .send(RegisteredSipEvent::HeartbeatHealthChanged { healthy: true })
            .await
            .unwrap();
        incoming
            .send(RegisteredSipEvent::KeepAlive {
                connection: SipConnectionId::Primary,
            })
            .await
            .unwrap();
        drop(incoming);
        let actor =
            tokio::spawn(async move { run(driver, &key, permit, lost, input, events).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while reads.load(Ordering::SeqCst) < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!actor.is_finished());
        assert_eq!(
            cleaned.load(Ordering::SeqCst),
            0,
            "EOF cannot discard buffered events"
        );
        for _ in 0..CAPACITY {
            assert!(matches!(
                output.recv().await.unwrap().event,
                RegisteredSipEvent::KeepAlive { .. }
            ));
        }
        assert!(matches!(
            output.recv().await.unwrap().event,
            RegisteredSipEvent::HeartbeatHealthChanged { healthy: false }
        ));
        assert!(matches!(
            output.recv().await.unwrap().event,
            RegisteredSipEvent::HeartbeatHealthChanged { healthy: true }
        ));
        assert!(matches!(
            output.recv().await.unwrap().event,
            RegisteredSipEvent::KeepAlive { .. }
        ));
        let exit = actor.await.unwrap();
        assert_eq!(exit.reason, StopReason::StreamClosed);
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
        drop(commands);
    }

    #[tokio::test]
    async fn terminal_backpressure_is_bounded_and_retains_exact_undelivered_events() {
        let (driver, incoming, _writes, cleaned) = fixture();
        let (commands, input, events, _output) = channels();
        let key = key();
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor.reserve(key.clone()).unwrap();
        let generation = permit.generation();
        let (_lease, lost) = watch::channel(false);
        for _ in 0..CAPACITY {
            events
                .send(Event {
                    generation,
                    reply_state: IncomingReply::Unanswered,
                    event: RegisteredSipEvent::KeepAlive {
                        connection: SipConnectionId::Primary,
                    },
                })
                .await
                .unwrap();
        }
        incoming
            .send(RegisteredSipEvent::HeartbeatHealthChanged { healthy: false })
            .await
            .unwrap();
        drop(incoming);
        let started = tokio::time::Instant::now();
        let exit = tokio::time::timeout(
            TERMINAL_DRAIN + Duration::from_secs(2),
            run(driver, &key, permit, lost, input, events),
        )
        .await
        .expect("terminal drain must not retain physical owner forever");
        assert!(started.elapsed() >= TERMINAL_DRAIN);
        assert_eq!(exit.reason, StopReason::StreamClosed);
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
        assert_eq!(exit.undelivered.len(), 1);
        assert_eq!(exit.undelivered[0].generation, generation);
        assert!(matches!(
            exit.undelivered[0].event,
            RegisteredSipEvent::HeartbeatHealthChanged { healthy: false }
        ));
        drop(commands);
    }
}
