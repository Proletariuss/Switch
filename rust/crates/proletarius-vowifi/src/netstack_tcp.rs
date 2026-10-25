//! AsyncRead/AsyncWrite stream backed solely by a session's smoltcp socket.
use crate::netstack_runtime::{NetworkRuntime, PacketDirection, PacketTransformOutcome};
use smoltcp::{
    iface::{Interface, SocketHandle, SocketSet},
    socket::tcp,
    wire::IpEndpoint,
};
use std::{
    convert::Infallible,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf},
    sync::{mpsc, oneshot, watch},
};
use zeroize::Zeroizing;

const BUFFER: usize = 16 * 1024;
const DYNAMIC_SOCKET_CAPACITY: usize = 16;
const COMMAND_CAPACITY: usize = DYNAMIC_SOCKET_CAPACITY;
const COMMANDS_PER_SERVICE: usize = DYNAMIC_SOCKET_CAPACITY;
const WRITE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct TcpFailure {
    kind: io::ErrorKind,
    message: String,
}
impl TcpFailure {
    fn new(kind: io::ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    fn error(&self) -> io::Error {
        io::Error::new(self.kind, self.message.clone())
    }
}
#[derive(Default)]
struct WriteProgress {
    accepted: u64,
    submitted: u64,
    acknowledged: u64,
    failure: Option<TcpFailure>,
    retired: bool,
    established: bool,
    waker: Option<Waker>,
    flush_deadline: Option<tokio::time::Instant>,
}
impl WriteProgress {
    fn fail(&mut self, error: TcpFailure) {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        self.wake();
    }
    fn wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}
type Progress = Arc<Mutex<WriteProgress>>;

struct NetworkWake(watch::Sender<u64>);
impl Wake for NetworkWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.send_modify(|n| *n = n.wrapping_add(1));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SocketIdentity {
    generation: u64,
    handle: SocketHandle,
}

enum TcpCommand {
    Dial {
        peer: SocketAddr,
        local_port: u16,
        keepalive: Option<Duration>,
        reply: oneshot::Sender<io::Result<PendingTcpStream>>,
    },
    Listen {
        local_port: u16,
        expected_peer: Option<SocketAddr>,
        keepalive: Option<Duration>,
        reply: oneshot::Sender<io::Result<PendingTcpListener>>,
    },
}

/// Bounded command endpoint for the single task which owns the userspace TCP
/// stack. Command completion means that a socket has been created (and, for a
/// listener, `listen` has succeeded); the TCP handshake is a separate await.
#[derive(Clone)]
pub struct TcpControl {
    keepalive: Option<Duration>,
    commands: mpsc::Sender<TcpCommand>,
    wake: watch::Sender<u64>,
}

impl TcpControl {
    /// Freeze per-session keepalive without changing the underlying registry.
    /// New dials, listeners and listener re-arms inherit this exact policy.
    pub fn with_keepalive(&self, interval: Duration) -> io::Result<Self> {
        if interval.is_zero() || interval.as_micros() > i64::MAX as u128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid TCP keepalive interval",
            ));
        }
        Ok(Self {
            keepalive: Some(interval),
            ..self.clone()
        })
    }
    /// Channel identity binds controls to one registry, independent of matching
    /// IP/ports or a cloned wake sender.
    pub fn same_registry(&self, other: &Self) -> bool {
        self.commands.same_channel(&other.commands)
    }

    /// True after the registry's command receiver has retired. Ownership
    /// identity remains stable, but no new operation can be accepted.
    pub fn is_closed(&self) -> bool {
        self.commands.is_closed()
    }

    /// Wait until the registry retires, including closure before this wait starts.
    pub async fn closed(&self) {
        self.commands.closed().await;
    }

    pub async fn dial(&self, peer: SocketAddr, local_port: u16) -> io::Result<PendingTcpStream> {
        validate_dial(peer, local_port)?;
        let (reply, result) = oneshot::channel();
        self.commands
            .send(TcpCommand::Dial {
                peer,
                local_port,
                keepalive: self.keepalive,
                reply,
            })
            .await
            .map_err(|_| runtime_closed())?;
        self.notify();
        result.await.map_err(|_| runtime_closed())?
    }

    pub async fn listen(
        &self,
        local_port: u16,
        expected_peer: Option<SocketAddr>,
    ) -> io::Result<PendingTcpListener> {
        validate_listener(local_port, expected_peer)?;
        let (reply, result) = oneshot::channel();
        self.commands
            .send(TcpCommand::Listen {
                local_port,
                expected_peer,
                keepalive: self.keepalive,
                reply,
            })
            .await
            .map_err(|_| runtime_closed())?;
        self.notify();
        result.await.map_err(|_| runtime_closed())?
    }

    fn notify(&self) {
        self.wake
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

fn runtime_closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "native TCP runtime is closed")
}

fn validate_dial(peer: SocketAddr, local_port: u16) -> io::Result<()> {
    if peer.port() == 0 || local_port == 0 || peer.ip().is_unspecified() || peer.ip().is_multicast()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid native TCP dial endpoint",
        ));
    }
    Ok(())
}

fn validate_listener(local_port: u16, expected_peer: Option<SocketAddr>) -> io::Result<()> {
    if local_port == 0
        || expected_peer.is_some_and(|peer| {
            peer.port() == 0 || peer.ip().is_unspecified() || peer.ip().is_multicast()
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid native TCP listener endpoint",
        ));
    }
    Ok(())
}

#[derive(Debug)]
enum CloseMode {
    Abort,
    Drain,
}

struct SocketLease {
    close: Option<oneshot::Sender<CloseMode>>,
    closed: Option<oneshot::Receiver<io::Result<()>>>,
    wake: watch::Sender<u64>,
    progress: Progress,
    flush_timer: Option<Pin<Box<tokio::time::Sleep>>>,
    managed: bool,
}

impl Drop for SocketLease {
    fn drop(&mut self) {
        if let Some(close) = self.close.take() {
            let _ = close.send(if self.managed {
                CloseMode::Drain
            } else {
                CloseMode::Abort
            });
            self.wake
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
    }
}

impl SocketLease {
    fn begin_close(&mut self) -> PendingTcpClose {
        if let Some(close) = self.close.take() {
            let _ = close.send(if self.managed {
                CloseMode::Drain
            } else {
                CloseMode::Abort
            });
            self.wake
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
        match self.closed.take() {
            Some(closed) => PendingTcpClose {
                closed: Some(closed),
                outcome: None,
            },
            None => PendingTcpClose {
                closed: None,
                outcome: Some(TcpCloseOutcome::Failed {
                    kind: io::ErrorKind::BrokenPipe,
                    message: "native TCP close acknowledgement is unavailable".to_owned(),
                    removal_confirmed: false,
                }),
            },
        }
    }

    async fn close(&mut self) -> io::Result<()> {
        self.begin_close().wait_cancel_safe().await
    }
}

enum TcpCloseOutcome {
    Completed,
    Failed {
        kind: io::ErrorKind,
        message: String,
        removal_confirmed: bool,
    },
}

impl TcpCloseOutcome {
    fn result(&self) -> io::Result<()> {
        match self {
            Self::Completed => Ok(()),
            Self::Failed { kind, message, .. } => Err(io::Error::new(*kind, message.clone())),
        }
    }
}

/// Move-only acknowledgement for one exact userspace TCP socket removal.
///
/// Creating this value has already synchronously signalled the socket entry
/// directly bound to the originating stream. Waiting is cancellation safe:
/// dropping a `wait_cancel_safe` future leaves the acknowledgement receiver in
/// this token, so another wait can resume. Once resolved, later waits replay
/// the same terminal outcome (`Ok(())` after removal, or the same error).
#[must_use = "wait for exact TCP socket removal before reusing its port or security state"]
pub struct PendingTcpClose {
    closed: Option<oneshot::Receiver<io::Result<()>>>,
    outcome: Option<TcpCloseOutcome>,
}

impl PendingTcpClose {
    /// A failed delivery may still have a confirmed local socket removal.
    /// A dropped acknowledgement channel never establishes this proof.
    pub fn removal_confirmed(&self) -> bool {
        matches!(
            &self.outcome,
            Some(TcpCloseOutcome::Completed)
                | Some(TcpCloseOutcome::Failed {
                    removal_confirmed: true,
                    ..
                })
        )
    }
    pub async fn wait_cancel_safe(&mut self) -> io::Result<()> {
        if let Some(outcome) = &self.outcome {
            return outcome.result();
        }

        let (result, removal_confirmed) = match self.closed.as_mut() {
            Some(closed) => match closed.await {
                Ok(result) => (result, true),
                Err(_) => (Err(runtime_closed()), false),
            },
            None => (Err(runtime_closed()), false),
        };
        self.closed = None;
        self.outcome = Some(match &result {
            Ok(()) => TcpCloseOutcome::Completed,
            Err(error) => TcpCloseOutcome::Failed {
                kind: error.kind(),
                message: error.to_string(),
                removal_confirmed,
            },
        });
        result
    }
}

/// Move-only pending outgoing stream. Dropping this value, including by
/// cancelling `connect`, signals only its directly bound socket entry.
pub struct PendingTcpStream {
    stream: DuplexStream,
    ready: oneshot::Receiver<io::Result<()>>,
    lease: SocketLease,
}

impl PendingTcpStream {
    pub async fn connect(self) -> io::Result<ManagedTcpStream> {
        let Self {
            stream,
            ready,
            mut lease,
        } = self;
        let established = ready.await.unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "native TCP dial ended before establishment",
            ))
        });
        if let Err(error) = established {
            // The ready error is terminal. Wait for exact-entry removal before
            // returning so a coordinator may safely clear an installed SA.
            let _ = lease.close().await;
            return Err(error);
        }
        lease.managed = true;
        Ok(ManagedTcpStream {
            stream,
            _lease: lease,
        })
    }

    /// Complete the handshake under the registration owner's absolute
    /// deadline and cancellation signal. Every terminal path waits for exact
    /// socket removal before returning an error.
    pub async fn connect_until(
        self,
        deadline: tokio::time::Instant,
        mut stop: watch::Receiver<bool>,
    ) -> io::Result<ManagedTcpStream> {
        let Self {
            stream,
            mut ready,
            mut lease,
        } = self;
        let established = loop {
            if *stop.borrow() || stop.has_changed().is_err() {
                break Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "native TCP dial cancelled",
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native TCP dial deadline exceeded",
                ));
            }
            tokio::select! {
                biased;
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break Err(io::Error::new(io::ErrorKind::Interrupted,"native TCP dial cancelled"));
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    break Err(io::Error::new(io::ErrorKind::TimedOut,"native TCP dial deadline exceeded"));
                }
                result = &mut ready => {
                    break result.unwrap_or_else(|_| Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "native TCP dial ended before establishment",
                    )));
                }
            }
        };
        if let Err(error) = established {
            let _ = lease.close().await;
            return Err(error);
        }
        lease.managed = true;
        Ok(ManagedTcpStream {
            stream,
            _lease: lease,
        })
    }

    /// Abort the pending socket and wait until the network owner has removed
    /// its exact entry from the SocketSet.
    pub async fn close(mut self) -> io::Result<()> {
        self.lease.close().await
    }
}

/// Move-only one-shot listener. It accepts at most one connection and may be
/// restricted to an exact peer address and source port. There is deliberately
/// no idle timeout while it waits for an incoming call. After accept/failure,
/// the registration coordinator must arm the next one-shot listener.
pub struct PendingTcpListener {
    stream: Option<DuplexStream>,
    ready: oneshot::Receiver<io::Result<()>>,
    lease: Option<SocketLease>,
    closing: Option<PendingTcpClose>,
    failure: Option<TcpFailure>,
}

/// Read-only local owner state; this never polls the accept receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpListenerState {
    Armed,
    Connected,
    Consumed,
    Closing,
    Retired,
}

impl PendingTcpListener {
    pub fn state(&self) -> TcpListenerState {
        if let Some(close) = &self.closing {
            return if close.removal_confirmed() {
                TcpListenerState::Retired
            } else {
                TcpListenerState::Closing
            };
        }
        let Some(lease) = &self.lease else {
            return TcpListenerState::Consumed;
        };
        let progress = lease.progress.lock().unwrap_or_else(|e| e.into_inner());
        if progress.retired {
            TcpListenerState::Retired
        } else if progress.established {
            TcpListenerState::Connected
        } else {
            TcpListenerState::Armed
        }
    }
    /// Wait for this one-shot listener without consuming its owner while the
    /// handshake is pending. Dropping the returned future is cancellation
    /// safe: the receiver, stream, and exact socket lease remain attached to
    /// `self`, so a later call can continue waiting for the same connection.
    ///
    /// A successful call consumes the one-shot listener state and returns its
    /// managed stream. A terminal listener error first waits for the network
    /// owner to remove the exact socket entry before it is returned.
    pub async fn accept_cancel_safe(&mut self) -> io::Result<ManagedTcpStream> {
        if self.closing.is_some() {
            return self.finish_failed_accept().await;
        }
        if self.stream.is_none() || self.lease.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "native TCP listener has already been consumed",
            ));
        }
        let established = (&mut self.ready).await.unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "native TCP listener ended before accept",
            ))
        });
        if let Err(error) = established {
            self.failure = Some(TcpFailure::new(error.kind(), error.to_string()));
            self.stream.take();
            let mut lease = self.lease.take().expect("checked listener lease");
            self.closing = Some(lease.begin_close());
            return self.finish_failed_accept().await;
        }
        let stream = self
            .stream
            .take()
            .expect("listener stream checked before accept");
        let mut lease = self
            .lease
            .take()
            .expect("listener lease checked before accept");
        lease.managed = true;
        Ok(ManagedTcpStream {
            stream,
            _lease: lease,
        })
    }

    async fn finish_failed_accept(&mut self) -> io::Result<ManagedTcpStream> {
        // This token remains attached to the listener if the borrowed accept
        // future is cancelled during failure cleanup. A later close cannot
        // forge success merely because the application half was already taken.
        self.closing
            .as_mut()
            .expect("failed accept owns cleanup")
            .wait_cancel_safe()
            .await?;
        Err(self
            .failure
            .as_ref()
            .expect("failed accept owns cause")
            .error())
    }

    /// Backwards-compatible consuming form for coordinators which do not need
    /// to race an incoming connection against other long-lived session work.
    pub async fn accept(mut self) -> io::Result<ManagedTcpStream> {
        self.accept_cancel_safe().await
    }

    /// Stop listening and wait until the exact socket is no longer owned by
    /// the network task.
    pub async fn close(mut self) -> io::Result<()> {
        // Closing a listener also makes its application half unavailable. The
        // exact socket lease is kept until its removal acknowledgement arrives.
        self.stream.take();
        if let Some(close) = self.closing.as_mut() {
            return close.wait_cancel_safe().await;
        }
        match self.lease.as_mut() {
            Some(lease) => lease.close().await,
            None => Ok(()),
        }
    }
}

/// Application half of a userspace TCP connection. The socket remains bound
/// to this move-only value; dropping it cannot delete a later socket which
/// happens to reuse the same smoltcp handle.
pub struct ManagedTcpStream {
    stream: DuplexStream,
    _lease: SocketLease,
}

impl ManagedTcpStream {
    /// Signal this stream's exact socket entry synchronously and return its
    /// cancellation-safe removal acknowledgement.
    pub fn begin_close(self) -> PendingTcpClose {
        let Self { stream, mut _lease } = self;
        let pending = _lease.begin_close();
        drop(stream);
        pending
    }

    /// Failed/cancelled protocol writes must not continue sending an unknown
    /// partial message. Abort only this captured socket; unacknowledged bytes
    /// remain an error in the removal receipt, never a delivery success.
    pub fn begin_abort(self) -> PendingTcpClose {
        let Self { stream, mut _lease } = self;
        _lease.managed = false;
        let pending = _lease.begin_close();
        drop(stream);
        pending
    }

    /// Drain this connection with a bounded deadline and wait for exact-entry removal. Session
    /// teardown uses this barrier before clearing the IMS security lease.
    pub async fn close(self) -> io::Result<()> {
        self.begin_close().wait_cancel_safe().await
    }
}

impl proletarius_ims::registration_connection::RegistrationTransportRetire for ManagedTcpStream {
    async fn retire(self) -> io::Result<()> {
        self.close().await
    }
}

impl AsyncRead for ManagedTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buffer)
    }
}

impl AsyncWrite for ManagedTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        // Admission and accepted-byte publication share the same short lock as
        // retirement; a concurrent remove cannot acknowledge an empty frontier
        // while this write is already entering the duplex buffer.
        let shared = self._lease.progress.clone();
        let mut progress = shared.lock().unwrap();
        if let Some(error) = progress.failure.as_ref() {
            return Poll::Ready(Err(error.error()));
        }
        if progress.retired {
            return Poll::Ready(Err(runtime_closed()));
        }
        let result = Pin::new(&mut self.stream).poll_write(cx, buffer);
        if let Poll::Ready(Ok(count)) = &result {
            progress.accepted = progress.accepted.saturating_add(*count as u64);
        }
        drop(progress);
        if matches!(result, Poll::Ready(Ok(_))) {
            self._lease
                .wake
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }

        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let deadline = {
            let mut progress = self._lease.progress.lock().unwrap();
            if let Some(error) = &progress.failure {
                return Poll::Ready(Err(error.error()));
            }
            if progress.acknowledged >= progress.accepted {
                progress.flush_deadline = None;
                drop(progress);
                self._lease.flush_timer = None;
                return Poll::Ready(Ok(()));
            }
            progress.waker = Some(cx.waker().clone());
            *progress
                .flush_deadline
                .get_or_insert_with(|| tokio::time::Instant::now() + WRITE_DRAIN_TIMEOUT)
        };
        if self
            ._lease
            .flush_timer
            .as_ref()
            .is_none_or(|timer| timer.deadline() != deadline)
        {
            self._lease.flush_timer = Some(Box::pin(tokio::time::sleep_until(deadline)));
        }
        self._lease
            .wake
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        if self
            ._lease
            .flush_timer
            .as_mut()
            .unwrap()
            .as_mut()
            .poll(cx)
            .is_ready()
        {
            let mut progress = self._lease.progress.lock().unwrap();
            if progress.acknowledged >= progress.accepted {
                progress.flush_deadline = None;
                return Poll::Ready(Ok(()));
            }
            let error = TcpFailure::new(
                io::ErrorKind::TimedOut,
                "native TCP write acknowledgement deadline exceeded",
            );
            progress.fail(error.clone());
            drop(progress);
            self._lease
                .wake
                .send_modify(|revision| *revision = revision.wrapping_add(1));
            return Poll::Ready(Err(error.error()));
        }
        Poll::Pending
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match Pin::new(&mut self.stream).poll_shutdown(cx) {
            Poll::Ready(Ok(())) => self.poll_flush(cx),
            other => other,
        }
    }
}

enum EntryMode {
    Dial {
        peer: IpEndpoint,
        local_port: u16,
    },
    Listen {
        local_port: u16,
        expected_peer: Option<IpEndpoint>,
    },
}

struct DynamicEntry {
    identity: SocketIdentity,
    mode: EntryMode,
    stream: DuplexStream,
    ready: Option<oneshot::Sender<io::Result<()>>>,
    waker: Waker,
    started: tokio::time::Instant,
    established: bool,
    input_closed: bool,
    output_closed: bool,
    close: oneshot::Receiver<CloseMode>,
    closed: Option<oneshot::Sender<io::Result<()>>>,
    progress: Progress,
    closing: Option<tokio::time::Instant>,
}

impl DynamicEntry {
    fn fail_ready(&mut self, error: io::Error) {
        if let Some(ready) = self.ready.take() {
            let _ = ready.send(Err(error));
        }
    }

    fn poll(&mut self, sockets: &mut SocketSet<'static>) -> bool {
        let socket = sockets.get_mut::<tcp::Socket>(self.identity.handle);
        if self.closing.is_none() {
            match self.close.try_recv() {
                Ok(CloseMode::Abort) | Err(oneshot::error::TryRecvError::Closed) => {
                    socket.abort();
                    return false;
                }
                Ok(CloseMode::Drain) => {
                    if !self.established {
                        socket.abort();
                        return false;
                    }
                    self.closing = Some(tokio::time::Instant::now() + WRITE_DRAIN_TIMEOUT);
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        let now = tokio::time::Instant::now();
        {
            let mut progress = self.progress.lock().unwrap();
            if progress.failure.is_some() {
                socket.abort();
                return false;
            }
            if self.closing.is_some_and(|deadline| now >= deadline)
                || progress.flush_deadline.is_some_and(|deadline| {
                    now >= deadline && progress.acknowledged < progress.accepted
                })
            {
                progress.fail(TcpFailure::new(
                    io::ErrorKind::TimedOut,
                    "native TCP write/close drain deadline exceeded",
                ));
                socket.abort();
                return false;
            }
        }
        if self.ready.as_ref().is_some_and(oneshot::Sender::is_closed) {
            socket.abort();
            return false;
        }

        if !self.established
            && matches!(
                socket.state(),
                tcp::State::Established | tcp::State::CloseWait
            )
        {
            if let EntryMode::Listen {
                expected_peer: Some(expected),
                ..
            } = self.mode
                && socket.remote_endpoint() != Some(expected)
            {
                socket.abort();
                self.fail_ready(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "native TCP listener received an unexpected peer",
                ));
                return false;
            }
            self.established = true;
            self.progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .established = true;
            if self
                .ready
                .take()
                .is_none_or(|ready| ready.send(Ok(())).is_err())
            {
                socket.abort();
                return false;
            }
        }

        if !self.established {
            if socket.state() == tcp::State::Closed {
                self.fail_ready(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "native TCP peer refused or reset the connection",
                ));
                return false;
            }
            if matches!(self.mode, EntryMode::Dial { .. })
                && self.started.elapsed() >= Duration::from_secs(30)
            {
                socket.abort();
                self.fail_ready(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native TCP handshake timed out",
                ));
                return false;
            }
            return true;
        }
        {
            let mut progress = self.progress.lock().unwrap();
            let acknowledged = progress
                .submitted
                .saturating_sub(socket.send_queue() as u64);
            if acknowledged > progress.acknowledged {
                progress.acknowledged = acknowledged;
                progress.wake();
            }
        }
        if socket.state() == tcp::State::Closed {
            let mut progress = self.progress.lock().unwrap();
            if progress.acknowledged < progress.accepted {
                progress.fail(TcpFailure::new(
                    io::ErrorKind::ConnectionReset,
                    "native TCP closed before queued writes were acknowledged",
                ));
            }
            return false;
        }
        if self.closing.is_some() && self.input_closed && {
            let progress = self.progress.lock().unwrap();
            progress.acknowledged >= progress.accepted
        } {
            // EOF was consumed on a previous service turn. The intervening
            // interface poll had a chance to send FIN, but a remote FIN ACK is
            // not a local removal proof (the peer may already have retired its
            // security SA). Every application byte is already TCP-ACKed.
            return false;
        }

        let mut cx = Context::from_waker(&self.waker);
        if self.closing.is_some() && socket.can_recv() {
            let _ = socket.recv(|data| (data.len(), ()));
        }
        if self.closing.is_none()
            && socket.can_recv()
            && socket
                .recv(
                    |data| match Pin::new(&mut self.stream).poll_write(&mut cx, data) {
                        Poll::Ready(Ok(n)) => (n, Ok(())),
                        Poll::Ready(Err(error)) => (0, Err(error)),
                        Poll::Pending => (0, Ok(())),
                    },
                )
                .is_err()
        {
            socket.abort();
            return false;
        }
        if self.closing.is_none()
            && !socket.may_recv()
            && !self.output_closed
            && let Poll::Ready(result) = Pin::new(&mut self.stream).poll_shutdown(&mut cx)
        {
            if result.is_err() {
                socket.abort();
                return false;
            }
            self.output_closed = true;
        }
        if socket.can_send() && !self.input_closed {
            let mut eof = false;
            let sent = socket.send(|space| {
                let mut buffer = ReadBuf::new(space);
                match Pin::new(&mut self.stream).poll_read(&mut cx, &mut buffer) {
                    Poll::Ready(Ok(())) => {
                        let length = buffer.filled().len();
                        eof = length == 0;
                        self.progress.lock().unwrap().submitted += length as u64;
                        (length, Ok(()))
                    }
                    Poll::Ready(Err(error)) => (0, Err(error)),
                    Poll::Pending => (0, Ok(())),
                }
            });
            if sent.is_err() {
                socket.abort();
                return false;
            }
            if eof {
                self.input_closed = true;
                socket.close();
                self.waker.wake_by_ref();
            }
        }
        true
    }
}

/// Dynamic socket registry owned and serviced by exactly one tunnel network
/// task. It never turns a single TCP connection failure into a tunnel error.
pub struct TcpSocketRegistry {
    commands: mpsc::Receiver<TcpCommand>,
    wake: watch::Sender<u64>,
    next_generation: u64,
    entries: Vec<DynamicEntry>,
}

impl TcpSocketRegistry {
    pub fn new(wake: watch::Sender<u64>) -> (TcpControl, Self) {
        let (commands, command_receiver) = mpsc::channel(COMMAND_CAPACITY);
        (
            TcpControl {
                keepalive: None,
                commands,
                wake: wake.clone(),
            },
            Self {
                commands: command_receiver,
                wake,
                next_generation: 1,
                entries: Vec::with_capacity(DYNAMIC_SOCKET_CAPACITY),
            },
        )
    }

    pub fn service(&mut self, interface: &mut Interface, sockets: &mut SocketSet<'static>) {
        self.apply_commands(interface, sockets);

        let mut index = 0;
        while index < self.entries.len() {
            if self.entries[index].poll(sockets) {
                index += 1;
            } else {
                self.remove_entry(index, sockets);
            }
        }
    }

    /// Retire the command plane and physically remove every owned socket.
    /// Close receipts acknowledge removal, but queued writes without a peer TCP
    /// ACK are explicitly failed. No cancellation path may forge delivery.
    pub fn retire(&mut self, sockets: &mut SocketSet<'static>) {
        while !self.entries.is_empty() {
            self.remove_entry(0, sockets);
        }
        self.commands.close();
        while self.commands.try_recv().is_ok() {}
    }

    fn next_identity(&mut self, handle: SocketHandle) -> SocketIdentity {
        let identity = SocketIdentity {
            generation: self.next_generation,
            handle,
        };
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        identity
    }

    fn apply_commands(&mut self, interface: &mut Interface, sockets: &mut SocketSet<'static>) {
        for _ in 0..COMMANDS_PER_SERVICE {
            let command = match self.commands.try_recv() {
                Ok(command) => command,
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    break;
                }
            };
            match command {
                TcpCommand::Dial {
                    peer,
                    local_port,
                    keepalive,
                    reply,
                } => self.create_dial(interface, sockets, peer, local_port, keepalive, reply),
                TcpCommand::Listen {
                    local_port,
                    expected_peer,
                    keepalive,
                    reply,
                } => self.create_listener(sockets, local_port, expected_peer, keepalive, reply),
            }
        }
    }

    fn has_capacity(&self, sockets: &SocketSet<'static>) -> bool {
        self.entries.len() < DYNAMIC_SOCKET_CAPACITY
            && sockets.iter().count() < DYNAMIC_SOCKET_CAPACITY
    }

    fn dial_conflicts(&self, peer: IpEndpoint, local_port: u16) -> bool {
        self.entries.iter().any(|entry| {
            matches!(
                entry.mode,
                EntryMode::Dial {
                    peer: existing_peer,
                    local_port: existing_port,
                } if existing_peer == peer && existing_port == local_port
            )
        })
    }

    fn listener_conflicts(&self, local_port: u16) -> bool {
        self.entries.iter().any(|entry| {
            !entry.established
                && matches!(
                    entry.mode,
                    EntryMode::Listen {
                        local_port: existing_port,
                        ..
                    } if existing_port == local_port
                )
        })
    }

    fn create_dial(
        &mut self,
        interface: &mut Interface,
        sockets: &mut SocketSet<'static>,
        peer: SocketAddr,
        local_port: u16,
        keepalive: Option<Duration>,
        reply: oneshot::Sender<io::Result<PendingTcpStream>>,
    ) {
        if reply.is_closed() {
            return;
        }
        if !self.has_capacity(sockets) {
            let _ = reply.send(Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "native TCP socket capacity reached",
            )));
            return;
        }
        let endpoint = IpEndpoint::new(peer.ip().into(), peer.port());
        if self.dial_conflicts(endpoint, local_port) {
            let _ = reply.send(Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "native TCP four-tuple is already active",
            )));
            return;
        }
        let mut socket = new_socket_with_keepalive(keepalive);
        if socket
            .connect(interface.context(), endpoint, local_port)
            .is_err()
        {
            let _ = reply.send(Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "native TCP source/remote unavailable",
            )));
            return;
        }
        let handle = sockets.add(socket);
        let identity = self.next_identity(handle);
        let (application, stream) = tokio::io::duplex(BUFFER);
        let (ready, receiver) = oneshot::channel();
        let (close, close_receiver) = oneshot::channel();
        let (closed, closed_receiver) = oneshot::channel();
        let progress = Arc::new(Mutex::new(WriteProgress::default()));
        self.entries.push(DynamicEntry {
            identity,
            mode: EntryMode::Dial {
                peer: endpoint,
                local_port,
            },
            stream,
            ready: Some(ready),
            waker: Waker::from(Arc::new(NetworkWake(self.wake.clone()))),
            started: tokio::time::Instant::now(),
            established: false,
            input_closed: false,
            output_closed: false,
            close: close_receiver,
            closed: Some(closed),
            progress: progress.clone(),
            closing: None,
        });
        let pending = PendingTcpStream {
            stream: application,
            ready: receiver,
            lease: SocketLease {
                close: Some(close),
                closed: Some(closed_receiver),
                wake: self.wake.clone(),
                progress,
                flush_timer: None,
                managed: false,
            },
        };
        if let Err(returned) = reply.send(Ok(pending)) {
            drop(returned);
            if let Some(index) = self
                .entries
                .iter()
                .position(|entry| entry.identity == identity)
            {
                self.remove_entry(index, sockets);
            }
        }
    }

    fn create_listener(
        &mut self,
        sockets: &mut SocketSet<'static>,
        local_port: u16,
        expected_peer: Option<SocketAddr>,
        keepalive: Option<Duration>,
        reply: oneshot::Sender<io::Result<PendingTcpListener>>,
    ) {
        if reply.is_closed() {
            return;
        }
        if !self.has_capacity(sockets) {
            let _ = reply.send(Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "native TCP socket capacity reached",
            )));
            return;
        }
        if self.listener_conflicts(local_port) {
            let _ = reply.send(Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "native TCP listener port is already armed",
            )));
            return;
        }
        let mut socket = new_socket_with_keepalive(keepalive);
        if socket.listen(local_port).is_err() {
            let _ = reply.send(Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "native TCP listener unavailable",
            )));
            return;
        }
        let handle = sockets.add(socket);
        let identity = self.next_identity(handle);
        let (application, stream) = tokio::io::duplex(BUFFER);
        let (ready, receiver) = oneshot::channel();
        let (close, close_receiver) = oneshot::channel();
        let (closed, closed_receiver) = oneshot::channel();
        let progress = Arc::new(Mutex::new(WriteProgress::default()));
        self.entries.push(DynamicEntry {
            identity,
            mode: EntryMode::Listen {
                local_port,
                expected_peer: expected_peer
                    .map(|peer| IpEndpoint::new(peer.ip().into(), peer.port())),
            },
            stream,
            ready: Some(ready),
            waker: Waker::from(Arc::new(NetworkWake(self.wake.clone()))),
            started: tokio::time::Instant::now(),
            established: false,
            input_closed: false,
            output_closed: false,
            close: close_receiver,
            closed: Some(closed),
            progress: progress.clone(),
            closing: None,
        });
        let pending = PendingTcpListener {
            stream: Some(application),
            ready: receiver,
            lease: Some(SocketLease {
                close: Some(close),
                closed: Some(closed_receiver),
                wake: self.wake.clone(),
                progress,
                flush_timer: None,
                managed: false,
            }),
            closing: None,
            failure: None,
        };
        if let Err(returned) = reply.send(Ok(pending)) {
            drop(returned);
            if let Some(index) = self
                .entries
                .iter()
                .position(|entry| entry.identity == identity)
            {
                self.remove_entry(index, sockets);
            }
        }
    }

    fn remove_entry(&mut self, index: usize, sockets: &mut SocketSet<'static>) {
        let mut entry = self.entries.swap_remove(index);
        let socket = sockets.get_mut::<tcp::Socket>(entry.identity.handle);
        socket.abort();
        sockets.remove(entry.identity.handle);
        entry.fail_ready(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "native TCP socket was cancelled",
        ));
        let result = {
            let mut progress = entry.progress.lock().unwrap();
            if progress.acknowledged < progress.accepted && progress.failure.is_none() {
                progress.fail(TcpFailure::new(
                    io::ErrorKind::BrokenPipe,
                    "native TCP socket removed with unacknowledged writes",
                ));
            }
            let result = progress
                .failure
                .as_ref()
                .map_or(Ok(()), |error| Err(error.error()));
            progress.retired = true;
            progress.wake();
            result
        };
        if let Some(closed) = entry.closed.take() {
            let _ = closed.send(result);
        }
    }
}

impl NetworkRuntime for TcpSocketRegistry {
    type ApplicationError = Infallible;

    fn retire(&mut self, sockets: &mut SocketSet<'static>) {
        TcpSocketRegistry::retire(self, sockets);
    }

    fn service(
        &mut self,
        interface: &mut Interface,
        sockets: &mut SocketSet<'static>,
    ) -> Result<(), Self::ApplicationError> {
        TcpSocketRegistry::service(self, interface, sockets);
        Ok(())
    }

    fn transform_packet(
        &mut self,
        _direction: PacketDirection,
        packet: Zeroizing<Vec<u8>>,
    ) -> Result<PacketTransformOutcome, io::Error> {
        Ok(PacketTransformOutcome::Forward(packet))
    }
}

#[cfg(test)]
fn new_socket() -> tcp::Socket<'static> {
    new_socket_with_keepalive(None)
}

fn new_socket_with_keepalive(keepalive: Option<Duration>) -> tcp::Socket<'static> {
    let mut socket = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; BUFFER]),
        tcp::SocketBuffer::new(vec![0; BUFFER]),
    );
    // The application may consume its last response and immediately retire
    // IMS. Delaying its TCP ACK until after that retirement loses the peer's
    // confirmed write even though the response reached the application.
    socket.set_ack_delay(None);
    socket.set_keep_alive(
        keepalive.map(|interval| smoltcp::time::Duration::from_micros(interval.as_micros() as u64)),
    );
    socket
}

#[cfg(test)]
struct ConnectingTcpStream {
    stream: DuplexStream,
    ready: oneshot::Receiver<io::Result<()>>,
}
#[cfg(test)]
impl ConnectingTcpStream {
    /// Resolves only after the TCP handshake, never merely after queueing SYN.
    async fn connect(self) -> io::Result<DuplexStream> {
        self.ready.await.map_err(|_| {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "native TCP connection ended before establishment",
            )
        })??;
        Ok(self.stream)
    }
}

#[cfg(test)]
struct TcpStreamDriver {
    handle: SocketHandle,
    peer: IpEndpoint,
    local_port: u16,
    stream: DuplexStream,
    ready: Option<oneshot::Sender<io::Result<()>>>,
    waker: Waker,
    started: Option<tokio::time::Instant>,
    established: bool,
    input_closed: bool,
    output_closed: bool,
}

#[cfg(test)]
impl TcpStreamDriver {
    /// Install before starting TunnelNetwork. No system socket is opened.
    /// Pass into_service() and the wake RX to that network's run method.
    fn install(
        sockets: &mut SocketSet<'static>,
        peer: SocketAddr,
        local_port: u16,
    ) -> io::Result<(Self, ConnectingTcpStream, watch::Receiver<u64>)> {
        if sockets.iter().count() >= 16
            || peer.port() == 0
            || local_port == 0
            || peer.ip().is_unspecified()
            || peer.ip().is_multicast()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid native TCP endpoint or socket capacity",
            ));
        }
        let handle = sockets.add(tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; BUFFER]),
            tcp::SocketBuffer::new(vec![0; BUFFER]),
        ));
        let (application, stream) = tokio::io::duplex(BUFFER);
        let (ready, receiver) = oneshot::channel();
        let (wake_tx, wake_rx) = watch::channel(0);
        let waker = Waker::from(Arc::new(NetworkWake(wake_tx)));
        Ok((
            Self {
                handle,
                peer: IpEndpoint::new(peer.ip().into(), peer.port()),
                local_port,
                stream,
                ready: Some(ready),
                waker,
                started: None,
                established: false,
                input_closed: false,
                output_closed: false,
            },
            ConnectingTcpStream {
                stream: application,
                ready: receiver,
            },
            wake_rx,
        ))
    }

    /// Transfer ownership into the network future, so its termination drops
    /// the pending-connect sender and duplex endpoint, waking application IO.
    fn into_service(
        mut self,
    ) -> impl FnMut(&mut Interface, &mut SocketSet<'static>) -> io::Result<()> {
        move |interface, sockets| {
            let result = self.poll(interface, sockets);
            if let Err(error) = &result
                && let Some(ready) = self.ready.take()
            {
                let _ = ready.send(Err(io::Error::new(error.kind(), error.to_string())));
            }
            result
        }
    }

    fn poll(
        &mut self,
        interface: &mut Interface,
        sockets: &mut SocketSet<'static>,
    ) -> io::Result<()> {
        let socket = sockets.get_mut::<tcp::Socket>(self.handle);
        if self.ready.as_ref().is_some_and(|ready| ready.is_closed()) {
            socket.abort();
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "native TCP connect cancelled",
            ));
        }
        if self.started.is_none() {
            socket
                .connect(interface.context(), self.peer, self.local_port)
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::AddrNotAvailable,
                        "native TCP source/remote unavailable",
                    )
                })?;
            self.started = Some(tokio::time::Instant::now());
        }
        if !self.established {
            if matches!(
                socket.state(),
                tcp::State::Established | tcp::State::CloseWait
            ) {
                self.established = true;
                if self.ready.take().unwrap().send(Ok(())).is_err() {
                    socket.abort();
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "native TCP consumer disappeared",
                    ));
                }
            } else if self.started.unwrap().elapsed() >= Duration::from_secs(30) {
                socket.abort();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native TCP connect timed out",
                ));
            }
        }
        if socket.state() == tcp::State::Closed && !(self.input_closed && self.output_closed) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "native TCP peer closed/reset",
            ));
        }
        let mut cx = Context::from_waker(&self.waker);
        if socket.can_recv() {
            socket
                .recv(
                    |data| match Pin::new(&mut self.stream).poll_write(&mut cx, data) {
                        Poll::Ready(Ok(n)) => (n, Ok(())),
                        Poll::Ready(Err(error)) => (0, Err(error)),
                        Poll::Pending => (0, Ok(())),
                    },
                )
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::ConnectionReset, "native TCP receive failed")
                })??;
        }
        if self.established
            && !socket.may_recv()
            && !self.output_closed
            && let Poll::Ready(result) = Pin::new(&mut self.stream).poll_shutdown(&mut cx)
        {
            result?;
            self.output_closed = true;
        }
        if socket.can_send() && !self.input_closed {
            let mut eof = false;
            socket
                .send(|space| {
                    let mut buffer = ReadBuf::new(space);
                    match Pin::new(&mut self.stream).poll_read(&mut cx, &mut buffer) {
                        Poll::Ready(Ok(())) => {
                            let n = buffer.filled().len();
                            eof = n == 0;
                            (n, Ok(()))
                        }
                        Poll::Ready(Err(error)) => (0, Err(error)),
                        Poll::Pending => (0, Ok(())),
                    }
                })
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "native TCP send failed")
                })??;
            if eof {
                self.input_closed = true;
                socket.close();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tcp_control_identity_and_runtime_liveness_are_independent() {
        let (wake, _receiver) = tokio::sync::watch::channel(0u64);
        let (control, registry) = super::TcpSocketRegistry::new(wake.clone());
        let clone = control.clone();
        let (other, other_registry) = super::TcpSocketRegistry::new(wake);
        assert!(control.same_registry(&clone));
        assert!(
            !control.same_registry(&other),
            "shared wake sender must not prove common registry"
        );
        assert!(!control.is_closed());
        drop(registry);
        assert!(control.is_closed());
        assert!(clone.is_closed());
        assert!(
            control.same_registry(&clone),
            "retirement must not alter provenance"
        );
        assert!(!other.is_closed());
        drop(other_registry);
        assert!(other.is_closed());
    }

    use super::*;
    use crate::{TunnelConfiguration, netstack_runtime::TunnelNetwork};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::mpsc,
    };

    #[tokio::test]
    async fn cancelled_listener_accept_can_resume_and_receive_connection() {
        let (application, mut peer) = tokio::io::duplex(BUFFER);
        let (ready_sender, ready) = oneshot::channel();
        let (close_sender, close_receiver) = oneshot::channel();
        let (closed_sender, closed_receiver) = oneshot::channel();
        let (wake, _wake_receiver) = watch::channel(0);
        let mut listener = PendingTcpListener {
            stream: Some(application),
            ready,
            lease: Some(SocketLease {
                close: Some(close_sender),
                closed: Some(closed_receiver),
                wake,
                progress: Arc::new(Mutex::new(WriteProgress::default())),
                flush_timer: None,
                managed: false,
            }),
            closing: None,
            failure: None,
        };

        {
            let mut cancelled = Box::pin(listener.accept_cancel_safe());
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(
                cancelled.as_mut().poll(&mut context),
                Poll::Pending
            ));
        }

        ready_sender.send(Ok(())).unwrap();
        let mut accepted = listener.accept_cancel_safe().await.unwrap();
        accepted.write_all(b"INVITE").await.unwrap();
        let mut message = [0; 6];
        peer.read_exact(&mut message).await.unwrap();
        assert_eq!(&message, b"INVITE");
        let second_error = match listener.accept_cancel_safe().await {
            Ok(_) => panic!("one-shot listener accepted twice"),
            Err(error) => error,
        };
        assert_eq!(second_error.kind(), io::ErrorKind::NotConnected);

        let removal = async {
            close_receiver.await.unwrap();
            closed_sender.send(Ok(())).unwrap();
        };
        let ((), close_result) = tokio::join!(removal, accepted.close());
        close_result.unwrap();
    }

    #[tokio::test]
    async fn cancelled_exact_close_wait_can_resume_before_immediate_port_reuse() {
        let (wake_tx, _wake) = watch::channel(0);
        let (_control, mut registry) = TcpSocketRegistry::new(wake_tx.clone());
        let mut sockets = SocketSet::new(vec![]);
        let local_port = 5060;

        let handle = sockets.add(new_socket());
        let identity = registry.next_identity(handle);
        let (application, network_stream) = tokio::io::duplex(BUFFER);
        let (close, close_receiver) = oneshot::channel();
        let (closed, closed_receiver) = oneshot::channel();
        registry.entries.push(DynamicEntry {
            identity,
            mode: EntryMode::Listen {
                local_port,
                expected_peer: None,
            },
            stream: network_stream,
            ready: None,
            waker: Waker::from(Arc::new(NetworkWake(wake_tx.clone()))),
            started: tokio::time::Instant::now(),
            established: false,
            input_closed: false,
            output_closed: false,
            close: close_receiver,
            closed: Some(closed),
            progress: Arc::new(Mutex::new(WriteProgress::default())),
            closing: None,
        });
        let stream = ManagedTcpStream {
            stream: application,
            _lease: SocketLease {
                close: Some(close),
                closed: Some(closed_receiver),
                wake: wake_tx,
                progress: Arc::new(Mutex::new(WriteProgress::default())),
                flush_timer: None,
                managed: false,
            },
        };
        assert!(registry.listener_conflicts(local_port));

        // begin_close signals the exact entry before returning. Cancelling
        // only the first acknowledgement wait must not lose its receiver.
        let mut pending_close = stream.begin_close();
        {
            let mut cancelled = Box::pin(pending_close.wait_cancel_safe());
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(
                cancelled.as_mut().poll(&mut context),
                Poll::Pending
            ));
        }

        assert!(!registry.entries[0].poll(&mut sockets));
        registry.remove_entry(0, &mut sockets);
        pending_close.wait_cancel_safe().await.unwrap();
        // Completion is explicit and idempotent for callers which need to
        // retry cleanup after a cancelled orchestration branch.
        pending_close.wait_cancel_safe().await.unwrap();
        assert!(registry.entries.is_empty());
        assert_eq!(sockets.iter().count(), 0);

        // Exact removal is a barrier: the same listen port can be allocated
        // immediately, without waiting for another network-service turn.
        let (reply, result) = oneshot::channel();
        registry.create_listener(&mut sockets, local_port, None, None, reply);
        let listener = result.await.unwrap().unwrap();
        assert_eq!(registry.entries.len(), 1);
        drop(listener);
        assert!(!registry.entries[0].poll(&mut sockets));
        registry.remove_entry(0, &mut sockets);
    }

    #[tokio::test]
    async fn late_close_signal_cannot_remove_reused_socket_handle() {
        let (wake_tx, _wake) = watch::channel(0);
        let (_control, mut registry) = TcpSocketRegistry::new(wake_tx.clone());
        let mut sockets = SocketSet::new(vec![]);

        let old_handle = sockets.add(new_socket());
        let old_identity = registry.next_identity(old_handle);
        let (_old_application, old_stream) = tokio::io::duplex(BUFFER);
        let (old_close, old_close_receiver) = oneshot::channel();
        let (old_closed, old_closed_receiver) = oneshot::channel();
        let old_lease = SocketLease {
            close: Some(old_close),
            closed: Some(old_closed_receiver),
            wake: wake_tx.clone(),
            progress: Arc::new(Mutex::new(WriteProgress::default())),
            flush_timer: None,
            managed: false,
        };
        registry.entries.push(DynamicEntry {
            identity: old_identity,
            mode: EntryMode::Dial {
                peer: IpEndpoint::new("10.0.0.9".parse::<std::net::IpAddr>().unwrap().into(), 5060),
                local_port: 5050,
            },
            stream: old_stream,
            ready: None,
            waker: Waker::from(Arc::new(NetworkWake(wake_tx.clone()))),
            started: tokio::time::Instant::now(),
            established: true,
            input_closed: false,
            output_closed: false,
            close: old_close_receiver,
            closed: Some(old_closed),
            progress: Arc::new(Mutex::new(WriteProgress::default())),
            closing: None,
        });
        registry.remove_entry(0, &mut sockets);

        let reused_handle = sockets.add(new_socket());
        assert_eq!(old_handle, reused_handle);
        let current_identity = registry.next_identity(reused_handle);
        assert_ne!(old_identity.generation, current_identity.generation);
        let (_application, stream) = tokio::io::duplex(BUFFER);
        let (current_close, current_close_receiver) = oneshot::channel();
        let (current_closed, current_closed_receiver) = oneshot::channel();
        let current_lease = SocketLease {
            close: Some(current_close),
            closed: Some(current_closed_receiver),
            wake: wake_tx.clone(),
            progress: Arc::new(Mutex::new(WriteProgress::default())),
            flush_timer: None,
            managed: false,
        };
        registry.entries.push(DynamicEntry {
            identity: current_identity,
            mode: EntryMode::Dial {
                peer: IpEndpoint::new("10.0.0.9".parse::<std::net::IpAddr>().unwrap().into(), 5060),
                local_port: 5050,
            },
            stream,
            ready: None,
            waker: Waker::from(Arc::new(NetworkWake(wake_tx))),
            started: tokio::time::Instant::now(),
            established: true,
            input_closed: false,
            output_closed: false,
            close: current_close_receiver,
            closed: Some(current_closed),
            progress: Arc::new(Mutex::new(WriteProgress::default())),
            closing: None,
        });

        // The old signal is wired directly to the removed entry, not routed
        // by the reusable SocketHandle. Dropping it cannot touch the new one.
        drop(old_lease);
        assert_eq!(registry.entries.len(), 1);
        assert_eq!(registry.entries[0].identity, current_identity);

        drop(current_lease);
        assert!(!registry.entries[0].poll(&mut sockets));
        registry.remove_entry(0, &mut sockets);
        assert!(registry.entries.is_empty());
        assert_eq!(sockets.iter().count(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dynamic_registry_isolates_failures_and_reclaims_cancelled_sockets() {
        let config = |ip: &str| TunnelConfiguration {
            ipv4_addresses: vec![ip.parse().unwrap()],
            ..Default::default()
        };
        let client_network = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
        let server_network = TunnelNetwork::new(&config("10.20.30.9")).unwrap();
        let (client_wake_tx, client_wake) = watch::channel(0);
        let (server_wake_tx, server_wake) = watch::channel(0);
        let (client_control, client_registry) = TcpSocketRegistry::new(client_wake_tx);
        let (server_control, server_registry) = TcpSocketRegistry::new(server_wake_tx);

        // Poll through command enqueue, then cancel before the registry can
        // acknowledge. The closed reply must make both commands no-ops when
        // their network tasks start.
        {
            let mut cancelled =
                Box::pin(client_control.dial("10.20.30.9:5098".parse().unwrap(), 29_998));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(
                cancelled.as_mut().poll(&mut context),
                Poll::Pending
            ));
        }
        {
            let mut cancelled = Box::pin(server_control.listen(5098, None));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(
                cancelled.as_mut().poll(&mut context),
                Poll::Pending
            ));
        }
        let (client_to_server, server_inbound) = mpsc::channel(256);
        let (server_to_client, client_inbound) = mpsc::channel(256);
        let (stop_tx, stop) = watch::channel(false);

        let client_runner = client_network.run_managed(
            SocketSet::new(vec![]),
            client_inbound,
            client_to_server,
            stop.clone(),
            client_wake,
            client_registry,
        );
        let server_runner = server_network.run_managed(
            SocketSet::new(vec![]),
            server_inbound,
            server_to_client,
            stop,
            server_wake,
            server_registry,
        );
        let application = async {
            // Each successful command ack creates a socket. Dropping its
            // pending value must reclaim it before repeated use can consume
            // the sixteen-socket capacity.
            for local_port in 30_000..30_100 {
                let pending = client_control
                    .dial("10.20.30.9:5099".parse().unwrap(), local_port)
                    .await
                    .unwrap();
                // Creating then dropping the handshake future models a
                // cancelled connect waiter, not merely an unused handle.
                drop(pending.connect());
            }

            // A real RST/refusal is scoped to just this socket; both network
            // tasks must remain available for the next dial/listen pair.
            let refused = client_control
                .dial("10.20.30.9:5099".parse().unwrap(), 31_000)
                .await
                .unwrap();
            let error = match refused.connect().await {
                Ok(_) => panic!("unopened port unexpectedly connected"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);

            let mut listener = server_control
                .listen(5060, Some("10.0.0.2:32000".parse().unwrap()))
                .await
                .unwrap();
            assert_eq!(listener.state(), TcpListenerState::Armed);

            // A long-lived session may poll the listener in `select!` and
            // cancel that wait whenever another event wins. Polling once and
            // dropping the future must preserve the same receiver and lease.
            {
                let mut cancelled = Box::pin(listener.accept_cancel_safe());
                let mut context = Context::from_waker(Waker::noop());
                assert!(matches!(
                    cancelled.as_mut().poll(&mut context),
                    Poll::Pending
                ));
            }
            let duplicate_listener = server_control.listen(5060, None).await;
            assert!(matches!(
                duplicate_listener,
                Err(error) if error.kind() == io::ErrorKind::AddrInUse
            ));
            let dial = client_control
                .dial("10.20.30.9:5060".parse().unwrap(), 32_000)
                .await
                .unwrap();
            let duplicate_dial = client_control
                .dial("10.20.30.9:5060".parse().unwrap(), 32_000)
                .await;
            assert!(matches!(
                duplicate_dial,
                Err(error) if error.kind() == io::ErrorKind::AddrInUse
            ));
            let mut client = dial.connect().await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), async {
                while listener.state() != TcpListenerState::Connected {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            for _ in 0..10 {
                assert_eq!(listener.state(), TcpListenerState::Connected);
            }
            let mut server = listener.accept_cancel_safe().await.unwrap();
            assert_eq!(listener.state(), TcpListenerState::Consumed);
            client.write_all(b"REGISTER").await.unwrap();
            let mut request = [0; 8];
            server.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"REGISTER");
            server.write_all(b"200 OK").await.unwrap();
            let mut response = [0; 6];
            client.read_exact(&mut response).await.unwrap();
            assert_eq!(&response, b"200 OK");
            client.close().await.unwrap();
            server.close().await.unwrap();
            stop_tx.send(true).unwrap();
        };

        let (client_result, server_result, ()) =
            tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(client_runner, server_runner, application)
            })
            .await
            .unwrap();
        client_result.unwrap();
        server_result.unwrap();
    }

    #[tokio::test]
    async fn listener_observation_retirement_is_exact_even_when_the_socket_handle_is_reused() {
        let (wake, _) = watch::channel(0);
        let (_, mut registry) = TcpSocketRegistry::new(wake);
        let mut sockets = SocketSet::new(vec![]);
        let (send, receive) = oneshot::channel();
        registry.create_listener(&mut sockets, 5063, None, None, send);
        let old = receive.await.unwrap().unwrap();
        let handle = registry.entries[0].identity.handle;
        assert_eq!(old.state(), TcpListenerState::Armed);
        registry.remove_entry(0, &mut sockets);
        assert_eq!(old.state(), TcpListenerState::Retired);
        let (send, receive) = oneshot::channel();
        registry.create_listener(&mut sockets, 5063, None, None, send);
        let new = receive.await.unwrap().unwrap();
        assert_eq!(registry.entries[0].identity.handle, handle);
        assert_eq!(old.state(), TcpListenerState::Retired);
        assert_eq!(new.state(), TcpListenerState::Armed);
        drop(old);
        assert_eq!(new.state(), TcpListenerState::Armed);
        drop(new);
        registry.remove_entry(0, &mut sockets);
    }

    #[tokio::test]
    async fn async_stream_crosses_ip_stack_with_segmentation() {
        stream_case(true, false).await;
        stream_case(false, false).await;
    }

    #[tokio::test]
    async fn async_sip_stream_crosses_installed_ims_packet_transform() {
        stream_case(true, true).await;
    }

    async fn stream_case(accept: bool, secured: bool) {
        use crate::netstack_runtime::{PacketDirection, PacketTransformOutcome};
        use proletarius_ims::{
            aka_exchange::SecurityKeys,
            security_offer::parse_security_server,
            security_policy::{LocalEndpoints, SecurityPolicy},
            security_transport::SecurityTransport,
        };
        use zeroize::Zeroizing;
        let transform = |client: bool| {
            let (own, peer, c, s, rc, rs, pc, ps, rpc, rps) = if client {
                (
                    "10.0.0.2",
                    "10.20.30.9",
                    111,
                    222,
                    333,
                    444,
                    5060,
                    5061,
                    5061,
                    5060,
                )
            } else {
                (
                    "10.20.30.9",
                    "10.0.0.2",
                    333,
                    444,
                    111,
                    222,
                    5061,
                    5060,
                    5060,
                    5061,
                )
            };
            let offer=parse_security_server(&format!("ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc;spi-c={rc};spi-s={rs};port-c={rpc};port-s={rps}")).unwrap().remove(0);
            let mut transport = SecurityTransport::new(
                SecurityPolicy::aes_sha1(
                    own.parse().unwrap(),
                    peer.parse().unwrap(),
                    LocalEndpoints {
                        spi_c: c,
                        spi_s: s,
                        port_c: pc,
                        port_s: ps,
                    },
                    &offer,
                    SecurityKeys {
                        ck: Zeroizing::new([1; 16]),
                        ik: Zeroizing::new([2; 16]),
                    },
                )
                .unwrap(),
            )
            .unwrap();
            move |direction: PacketDirection, packet: Zeroizing<Vec<u8>>| {
                if !secured {
                    return Ok(PacketTransformOutcome::Forward(packet));
                }
                let disposition = match direction {
                    PacketDirection::Inbound => transport.route_inbound(packet),
                    PacketDirection::Outbound => transport.route_outbound(packet),
                }
                .map_err(|e| io::Error::other(format!("{e:?}")))?;
                Ok(match disposition {
                    proletarius_ims::security_transport::TransportDisposition::Protected(
                        packet,
                    )
                    | proletarius_ims::security_transport::TransportDisposition::Passthrough(
                        packet,
                    ) => PacketTransformOutcome::Forward(packet),
                    proletarius_ims::security_transport::TransportDisposition::Drop(_) => {
                        PacketTransformOutcome::Discard
                    }
                })
            }
        };
        let config = |ip: &str| TunnelConfiguration {
            ipv4_addresses: vec![ip.parse().unwrap()],
            ..Default::default()
        };
        let a = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
        let b = TunnelNetwork::new(&config("10.20.30.9")).unwrap();
        let mut sa = SocketSet::new(vec![]);
        let (driver, pending, wake) =
            TcpStreamDriver::install(&mut sa, "10.20.30.9:5060".parse().unwrap(), 5060).unwrap();
        let mut sb = SocketSet::new(vec![]);
        let mut server = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; BUFFER]),
            tcp::SocketBuffer::new(vec![0; BUFFER]),
        );
        if accept {
            server.listen(5060).unwrap();
        }
        let handle = sb.add(server);
        let (ab, ab_rx) = mpsc::channel(4);
        let (ba, ba_rx) = mpsc::channel(4);
        if secured {
            ba.try_send(Zeroizing::new(vec![0; 20])).unwrap();
        }
        let (stop_tx, stop) = watch::channel(false);
        let (_server_wake, server_wake) = watch::channel(0);
        let client = a.run_transformed(
            sa,
            ba_rx,
            ab,
            stop.clone(),
            wake,
            driver.into_service(),
            transform(true),
        );
        let mut received = Vec::new();
        let mut sent = 0;
        let server = b.run_transformed(
            sb,
            ab_rx,
            ba,
            stop,
            server_wake,
            |_, sockets| {
                let socket = sockets.get_mut::<tcp::Socket>(handle);
                if socket.can_recv() {
                    socket
                        .recv(|data| {
                            received.extend_from_slice(data);
                            (data.len(), ())
                        })
                        .unwrap();
                }
                if socket.can_send() && sent < received.len() {
                    sent += socket.send_slice(&received[sent..]).unwrap();
                }
                Ok::<_, ()>(())
            },
            transform(false),
        );
        let application = async {
            if !accept {
                assert!(pending.connect().await.is_err());
                stop_tx.send(true).unwrap();
                return;
            }
            let mut stream = pending.connect().await.unwrap();
            let mut data =
                b"MESSAGE sip:ims.example SIP/2.0\r\nContent-Length: 5000\r\n\r\n".to_vec();
            data.extend((0..5000).map(|n| (n % 251) as u8));
            stream.write_all(&data).await.unwrap();
            {
                use proletarius_ims::stream::{SipStreamItem, SipStreamReader};
                let mut reader = SipStreamReader::new(&mut stream);
                let Some(SipStreamItem::Message(reply)) = reader.next().await.unwrap() else {
                    panic!("missing SIP frame");
                };
                assert_eq!(&*reply, &data);
            }
            stream.shutdown().await.unwrap();
            stop_tx.send(true).unwrap();
        };
        let (a, b, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(client, server, application)
        })
        .await
        .unwrap();
        if accept {
            let a = a.unwrap();
            let b = b.unwrap();
            assert_eq!(a.inbound_discarded, u64::from(secured));
            assert_eq!(b.inbound_discarded, 0);
            if secured {
                assert!(a.max_inbound_wire_bytes > 1280);
                assert!(a.max_inbound_wire_bytes <= 1320);
                assert!(b.max_outbound_wire_bytes > 1280);
                assert!(b.max_outbound_wire_bytes <= 1320);
            }
            assert_eq!(
                received.len(),
                5000 + b"MESSAGE sip:ims.example SIP/2.0\r\nContent-Length: 5000\r\n\r\n".len()
            );
        } else {
            assert!(matches!(
                a,
                Err(crate::netstack_runtime::NetworkRunError::Application(_))
            ));
            // Closing the client endpoint can reach the peer before its stop
            // watcher; either explicit cancellation or channel EOF is valid.
            assert!(
                b.is_ok()
                    || matches!(
                        b,
                        Err(crate::netstack_runtime::NetworkRunError::TunnelClosed)
                    )
            );
            assert!(received.is_empty());
        }
    }
    #[tokio::test]
    async fn session_keepalive_is_applied_by_same_registry_and_rearm_keeps_policy() {
        let (wake, _changes) = watch::channel(0);
        let (control, mut registry) = TcpSocketRegistry::new(wake);
        let interval = Duration::from_millis(125);
        let selected = control.with_keepalive(interval).unwrap();
        assert!(control.same_registry(&selected));
        assert!(control.with_keepalive(Duration::ZERO).is_err());
        let mut sockets = SocketSet::new(vec![]);
        for _ in 0..2 {
            let selected = selected.clone();
            let task = tokio::spawn(async move { selected.listen(5063, None).await });
            let TcpCommand::Listen {
                local_port,
                expected_peer,
                keepalive,
                reply,
            } = registry.commands.recv().await.unwrap()
            else {
                panic!("expected listener command")
            };
            registry.create_listener(&mut sockets, local_port, expected_peer, keepalive, reply);
            let listener = task.await.unwrap().unwrap();
            let (handle, _) = sockets.iter().next().unwrap();
            assert_eq!(
                sockets.get::<tcp::Socket>(handle).keep_alive(),
                Some(smoltcp::time::Duration::from_millis(125))
            );
            drop(listener);
            assert!(!registry.entries[0].poll(&mut sockets));
            registry.remove_entry(0, &mut sockets);
            assert_eq!(sockets.iter().count(), 0);
        }
    }
}

#[cfg(test)]
mod drain_tests;
