//! Runtime-installed IMS security transform owned by the tunnel network task.
use crate::netstack_runtime::{NetworkRuntime, PacketDirection, PacketTransformOutcome};
use proletarius_ims::{
    RegisterPhase, SipError, SipResponse,
    registration_connection::{
        InstalledExchangeFailure, InstalledRegistrationConnection,
        InstalledSecAgree as BoundSecAgree, ProtectedRegisterAttempt, ProtectedRegisterContext,
        ReadySecAgree, RetiredRegistrationTransport,
    },
    registration_io::RegisterWaitError,
    security_policy::SecurityPolicy,
    security_slot::{SecurityGeneration, SecuritySlot, SecuritySlotError},
    security_transport::TransportDisposition,
    stream::{SipStreamItem, SipStreamReader},
};
use smoltcp::iface::{Interface, SocketSet};
use std::{fmt, io};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf},
    sync::{mpsc, oneshot, watch},
    time::Instant,
};
use zeroize::Zeroizing;

const COMMAND_CAPACITY: usize = 8;
const COMMANDS_PER_SERVICE: usize = 8;

enum SecurityCommand {
    Install {
        policy: SecurityPolicy,
        reply: oneshot::Sender<Result<SecurityLease, SecuritySlotError>>,
    },
    InstallRegistration {
        ready: Box<ReadySecAgree>,
        reply: oneshot::Sender<Result<InstalledSecAgree, SecuritySlotError>>,
    },
}

enum CleanupCommand {
    Clear {
        generation: SecurityGeneration,
        reply: Option<oneshot::Sender<Result<(), SecuritySlotError>>>,
    },
}

#[derive(Debug)]
pub enum SecurityControlError {
    Closed,
    Slot(SecuritySlotError),
}

impl fmt::Display for SecurityControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("IMS security runtime is closed"),
            Self::Slot(error) => write!(formatter, "IMS security slot rejected command: {error:?}"),
        }
    }
}

impl std::error::Error for SecurityControlError {}

/// Cloneable command endpoint only; it cannot transform packets or inspect
/// keys. A successful install reply is the ordering barrier before opening the
/// protected port-c connection.
#[derive(Clone)]
pub struct SecurityControl {
    commands: mpsc::Sender<SecurityCommand>,
    wake: watch::Sender<u64>,
}

/// Move-only installed-SA ownership. Dropping the lease, including while an
/// install reply is still buffered in a cancelled oneshot, queues an exact
/// generation cleanup. This closes the ack-delivery cancellation race.
pub struct SecurityLease {
    generation: SecurityGeneration,
    cleanup: mpsc::UnboundedSender<CleanupCommand>,
    wake: watch::Sender<u64>,
    armed: bool,
}

/// Keeps the exact SA lease inseparable from the receipt-bound REGISTER
/// connection while the authenticated transaction is in flight.
pub struct LeasedRegistrationConnection<T> {
    registration: InstalledRegistrationConnection<T>,
    lease: SecurityLease,
}

pub struct LeasedExchangeFailure<T> {
    failure: Box<InstalledExchangeFailure<T>>,
    lease: SecurityLease,
}

pub struct InstalledSecAgree {
    proof: BoundSecAgree,
    lease: SecurityLease,
}

pub struct LeasedRegisteredParts<T> {
    pub(crate) reader: SipStreamReader<ReadHalf<T>>,
    pub(crate) writer: WriteHalf<T>,
    pub(crate) lease: SecurityLease,
}

type RetiredRegistrationWithLease<T> = (
    RegisterWaitError,
    RetiredRegistrationTransport<T>,
    SecurityLease,
);
type RegistrationRetirementFailure = Box<(SipError, SecurityLease)>;
type RetiredLeaseResult<T> =
    Result<(RetiredRegistrationTransport<T>, SecurityLease), RegistrationRetirementFailure>;

impl SecurityLease {
    pub fn generation(&self) -> SecurityGeneration {
        self.generation
    }

    pub async fn clear(mut self) -> Result<(), SecurityControlError> {
        let (reply, result) = oneshot::channel();
        self.cleanup
            .send(CleanupCommand::Clear {
                generation: self.generation(),
                reply: Some(reply),
            })
            .map_err(|_| SecurityControlError::Closed)?;
        self.notify();
        let cleared = result
            .await
            .map_err(|_| SecurityControlError::Closed)?
            .map_err(SecurityControlError::Slot);
        if cleared.is_ok() {
            self.armed = false;
        }
        cleared
    }

    fn notify(&self) {
        self.wake
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> LeasedRegistrationConnection<T> {
    pub(crate) fn with_retained_events(mut self) -> Self {
        self.registration = self.registration.with_retained_events();
        self
    }
    pub(crate) fn take_retained_events(&mut self) -> std::collections::VecDeque<SipStreamItem> {
        self.registration.take_retained_events()
    }

    pub fn phase(&self) -> RegisterPhase {
        self.registration.phase()
    }

    pub fn request_context(&self) -> ProtectedRegisterAttempt<'_> {
        self.registration.request_context()
    }

    pub fn next_cseq(&self) -> Result<u32, SipError> {
        self.registration.next_cseq()
    }

    pub async fn exchange_authenticated(
        self,
        request: &[u8],
        deadline: Instant,
        stop: watch::Receiver<bool>,
        events: &mpsc::Sender<SipStreamItem>,
    ) -> Result<(Self, SipResponse), Box<LeasedExchangeFailure<T>>> {
        let Self {
            registration,
            lease,
        } = self;
        match registration
            .exchange_authenticated(request, deadline, stop, events)
            .await
        {
            Ok((registration, response)) => Ok((
                Self {
                    registration,
                    lease,
                },
                response,
            )),
            Err(failure) => Err(Box::new(LeasedExchangeFailure {
                failure: Box::new(failure),
                lease,
            })),
        }
    }

    pub(crate) fn into_retired_and_lease(self) -> RetiredLeaseResult<T> {
        let Self {
            registration,
            lease,
        } = self;
        match registration.into_retired_transport() {
            Ok(retired) => Ok((retired, lease)),
            Err(error) => Err(Box::new((error, lease))),
        }
    }

    pub(crate) fn into_registered_parts(
        self,
    ) -> Result<LeasedRegisteredParts<T>, Box<(SipError, SecurityLease)>> {
        let Self {
            registration,
            lease,
        } = self;
        match registration.into_registered_parts() {
            Ok((reader, writer)) => Ok(LeasedRegisteredParts {
                reader,
                writer,
                lease,
            }),
            Err(error) => Err(Box::new((error, lease))),
        }
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> LeasedExchangeFailure<T> {
    pub fn error(&self) -> &RegisterWaitError {
        self.failure.error()
    }

    pub(crate) fn into_error_retired_and_lease(
        self,
    ) -> Result<RetiredRegistrationWithLease<T>, RegistrationRetirementFailure> {
        let Self { failure, lease } = self;
        match (*failure).into_error_and_retired() {
            Ok((error, retired)) => Ok((error, retired, lease)),
            Err(error) => Err(Box::new((error, lease))),
        }
    }
}

impl Drop for SecurityLease {
    fn drop(&mut self) {
        if self.armed
            && self
                .cleanup
                .send(CleanupCommand::Clear {
                    generation: self.generation(),
                    reply: None,
                })
                .is_ok()
        {
            self.notify();
        }
    }
}

impl SecurityControl {
    pub async fn install(
        &self,
        policy: SecurityPolicy,
    ) -> Result<SecurityLease, SecurityControlError> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(SecurityCommand::Install { policy, reply })
            .await
            .map_err(|_| SecurityControlError::Closed)?;
        self.notify();
        result
            .await
            .map_err(|_| SecurityControlError::Closed)?
            .map_err(SecurityControlError::Slot)
    }

    pub async fn install_registration(
        &self,
        ready: ReadySecAgree,
    ) -> Result<InstalledSecAgree, SecurityControlError> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(SecurityCommand::InstallRegistration {
                ready: Box::new(ready),
                reply,
            })
            .await
            .map_err(|_| SecurityControlError::Closed)?;
        self.notify();
        result
            .await
            .map_err(|_| SecurityControlError::Closed)?
            .map_err(SecurityControlError::Slot)
    }

    fn notify(&self) {
        self.wake
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

impl InstalledSecAgree {
    pub fn context(&self) -> &ProtectedRegisterContext {
        self.proof.context()
    }

    pub fn next_cseq(&self) -> Result<u32, SipError> {
        self.proof.next_cseq()
    }

    pub fn bind<T: AsyncRead + AsyncWrite + Unpin>(
        self,
        stream: T,
    ) -> LeasedRegistrationConnection<T> {
        LeasedRegistrationConnection {
            registration: self.proof.bind(stream),
            lease: self.lease,
        }
    }

    pub(crate) async fn rollback(self) -> Result<(), SecurityControlError> {
        let Self { proof, lease } = self;
        drop(proof);
        lease.clear().await
    }
}

/// Same-task owner for commands, sockets and packet transformation. It adds no
/// per-packet mutex or cross-process hop.
pub struct SecurityNetworkRuntime<F> {
    commands: mpsc::Receiver<SecurityCommand>,
    cleanup: mpsc::UnboundedReceiver<CleanupCommand>,
    cleanup_sender: mpsc::UnboundedSender<CleanupCommand>,
    wake: watch::Sender<u64>,
    slot: SecuritySlot,
    application: F,
}

impl<F> SecurityNetworkRuntime<F> {
    pub fn new(application: F, wake: watch::Sender<u64>) -> (SecurityControl, Self) {
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (cleanup_sender, cleanup) = mpsc::unbounded_channel();
        (
            SecurityControl {
                commands,
                wake: wake.clone(),
            },
            Self {
                commands: receiver,
                cleanup,
                cleanup_sender,
                wake,
                slot: SecuritySlot::new(),
                application,
            },
        )
    }

    /// Final owner-only retirement. Close admission before erasing the active
    /// keys; buffered installs never acquire an SA after this boundary.
    pub(crate) fn retire(&mut self) {
        self.commands.close();
        while let Ok(command) = self.commands.try_recv() {
            match command {
                SecurityCommand::Install { reply, .. } => {
                    let _ = reply.send(Err(SecuritySlotError::Empty));
                }
                SecurityCommand::InstallRegistration { reply, .. } => {
                    let _ = reply.send(Err(SecuritySlotError::Empty));
                }
            }
        }
        self.cleanup.close();
        while let Ok(CleanupCommand::Clear { generation, reply }) = self.cleanup.try_recv() {
            let result = self.slot.clear_if(generation);
            if let Some(reply) = reply {
                let _ = reply.send(result);
            }
        }
        self.slot = SecuritySlot::new();
    }

    fn apply_cleanup(&mut self) {
        for _ in 0..COMMANDS_PER_SERVICE {
            let cleanup = match self.cleanup.try_recv() {
                Ok(cleanup) => cleanup,
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    break;
                }
            };
            match cleanup {
                CleanupCommand::Clear { generation, reply } => {
                    let result = self.slot.clear_if(generation);
                    if let Some(reply) = reply {
                        let _ = reply.send(result);
                    }
                }
            }
        }
    }

    fn apply_commands(&mut self) {
        self.apply_cleanup();
        for _ in 0..COMMANDS_PER_SERVICE {
            let command = match self.commands.try_recv() {
                Ok(command) => command,
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    break;
                }
            };
            match command {
                SecurityCommand::Install { policy, reply } => {
                    let result = self.slot.install(policy).map(|receipt| SecurityLease {
                        generation: receipt.generation(),
                        cleanup: self.cleanup_sender.clone(),
                        wake: self.wake.clone(),
                        armed: true,
                    });
                    // A failed send drops the move-only lease, which queues
                    // generation-scoped cleanup through the separate channel.
                    let _ = reply.send(result);
                }
                SecurityCommand::InstallRegistration { ready, reply } => {
                    let result = self.slot.install_registration(*ready).map(|proof| {
                        let lease = SecurityLease {
                            generation: proof.generation(),
                            cleanup: self.cleanup_sender.clone(),
                            wake: self.wake.clone(),
                            armed: true,
                        };
                        InstalledSecAgree { proof, lease }
                    });
                    // A cancelled waiter drops the fused proof and lease; the
                    // lease queues generation-scoped cleanup before the next
                    // packet-owner poll.
                    let _ = reply.send(result);
                }
            }
        }
        // Process lease drops caused by cancelled install waiters before the
        // next interface poll can emit a packet under an orphaned SA.
        self.apply_cleanup();
    }
}

impl<F, E> NetworkRuntime for SecurityNetworkRuntime<F>
where
    F: FnMut(&mut Interface, &mut SocketSet<'static>) -> Result<(), E>,
{
    type ApplicationError = E;

    fn service(
        &mut self,
        interface: &mut Interface,
        sockets: &mut SocketSet<'static>,
    ) -> Result<(), Self::ApplicationError> {
        self.apply_commands();
        (self.application)(interface, sockets)
    }

    fn transform_packet(
        &mut self,
        direction: PacketDirection,
        packet: Zeroizing<Vec<u8>>,
    ) -> Result<PacketTransformOutcome, io::Error> {
        let retired_control = matches!(direction, PacketDirection::Outbound)
            && self.slot.exact_retired_tcp_control(&packet);
        let disposition = match direction {
            PacketDirection::Inbound => self.slot.route_inbound(packet),
            PacketDirection::Outbound => self.slot.route_outbound(packet),
        }
        .map_err(|error| io::Error::other(format!("IMS security transform failed: {error:?}")))?;
        Ok(match disposition {
            TransportDisposition::Protected(packet) | TransportDisposition::Passthrough(packet) => {
                PacketTransformOutcome::Forward(packet)
            }
            TransportDisposition::Drop(
                proletarius_ims::security_transport::InboundDropReason::RetiredSecurityAssociation,
            ) if retired_control => PacketTransformOutcome::RetiredControlDiscard,
            TransportDisposition::Drop(_) => PacketTransformOutcome::Discard,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        TunnelConfiguration, netstack_runtime::TunnelNetwork, netstack_tcp::TcpSocketRegistry,
    };
    use proletarius_ims::{
        SecAgreeMode,
        aka_authorization::AuthorizationRequest,
        aka_exchange::{SecurityKeys, SimAnswer, answer_challenge},
        registration_connection::{
            ChallengeResolution, RegistrationConnection, RegistrationProgress,
            RegistrationTransportRetire, SecAgreeParameters,
        },
        security_esp::Receiver,
        security_offer::{ClientSecurityMechanism, parse_security_server},
        security_policy::LocalEndpoints,
    };
    use smoltcp::socket::tcp;
    use std::{
        convert::Infallible,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        task::{Context, Poll},
        time::Duration,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadBuf};

    struct TestRegistrationStream(tokio::io::DuplexStream);

    impl AsyncRead for TestRegistrationStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_read(context, buffer)
        }
    }

    impl AsyncWrite for TestRegistrationStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(context, buffer)
        }

        fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(context)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(context)
        }
    }

    impl RegistrationTransportRetire for TestRegistrationStream {
        async fn retire(mut self) -> io::Result<()> {
            self.shutdown().await
        }
    }

    fn policy() -> SecurityPolicy {
        let offer = parse_security_server(
            "ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc;spi-c=111;spi-s=222;port-c=6050;port-s=6060",
        )
        .unwrap()
        .remove(0);
        SecurityPolicy::aes_sha1(
            "10.0.0.2".parse().unwrap(),
            "10.0.0.9".parse().unwrap(),
            LocalEndpoints {
                spi_c: 333,
                spi_s: 444,
                port_c: 5064,
                port_s: 5063,
            },
            &offer,
            SecurityKeys {
                ck: Zeroizing::new([1; 16]),
                ik: Zeroizing::new([2; 16]),
            },
        )
        .unwrap()
    }

    async fn ready_sec_agree() -> ReadySecAgree {
        const CALL_ID: &str = "cancelled-install";
        const REQUEST: &[u8] = b"REGISTER sip:ims.example SIP/2.0\r\nCall-ID: cancelled-install\r\nCSeq: 1 REGISTER\r\nContent-Length: 0\r\n\r\n";
        const SERVER_OFFER: &str = "ipsec-3gpp;q=0.98;alg=hmac-sha-1-96;ealg=aes-cbc;prot=esp;mod=trans;spi-c=111;spi-s=222;port-c=6064;port-s=6063";

        let (client, mut server) = tokio::io::duplex(4096);
        let connection = RegistrationConnection::new(
            TestRegistrationStream(client),
            CALL_ID,
            1,
            SecAgreeMode::Auto,
        )
        .unwrap();
        let (_stop_tx, stop) = watch::channel(false);
        let (events, _event_receiver) = mpsc::channel(4);
        let deadline = Instant::now() + Duration::from_secs(1);
        let remote = async {
            let mut request = vec![0; REQUEST.len()];
            server.read_exact(&mut request).await.unwrap();
            assert_eq!(request, REQUEST);
            server
                .write_all(
                    format!(
                        "SIP/2.0 401 Challenge\r\nCall-ID: {CALL_ID}\r\nCSeq: 1 REGISTER\r\nWWW-Authenticate: Digest realm=\"ims.example\",nonce=\"{}\",algorithm=AKAv1-MD5\r\nSecurity-Server: {SERVER_OFFER}\r\nContent-Length: 0\r\n\r\n",
                        "01".repeat(32)
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        };
        let local = connection.advance(REQUEST, deadline, stop.clone(), &events);
        let (progress, ()) = tokio::join!(local, remote);
        let RegistrationProgress::Challenge(challenge) = progress.unwrap() else {
            panic!("expected security challenge")
        };
        assert!(challenge.requires_security_install());
        let answer = answer_challenge(
            challenge.response(),
            &AuthorizationRequest {
                username: "user",
                method: "REGISTER",
                uri: "sip:ims.example",
                body: b"",
                cnonce: "fixed",
                nonce_count: 1,
            },
            deadline,
            stop,
            |rand, autn| async move {
                assert_eq!(*rand, [1; 16]);
                assert_eq!(*autn, [1; 16]);
                Ok::<_, Infallible>(SimAnswer::Success {
                    res: Zeroizing::new(vec![1; 8]),
                    ck: Zeroizing::new([2; 16]),
                    ik: Zeroizing::new([3; 16]),
                })
            },
        )
        .await
        .unwrap();
        let ChallengeResolution::Security(pending) = challenge.resolve(answer) else {
            panic!("successful AKA must produce a security transition")
        };
        let mechanisms = [ClientSecurityMechanism {
            algorithm: "hmac-sha-1-96",
            encryption: "aes-cbc",
            protocol: Some("esp"),
            mode: Some("trans"),
        }];
        pending
            .prepare(SecAgreeParameters {
                local_ip: "10.0.0.2".parse().unwrap(),
                remote_ip: "10.0.0.9".parse().unwrap(),
                local_endpoints: LocalEndpoints {
                    spi_c: 333,
                    spi_s: 444,
                    port_c: 5064,
                    port_s: 5063,
                },
                client_mechanisms: &mechanisms,
                strict: true,
            })
            .unwrap()
            .retire_plain()
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn install_ack_precedes_first_protected_connection_packet() {
        let network = TunnelNetwork::new(&TunnelConfiguration {
            ipv4_addresses: vec!["10.0.0.2".parse().unwrap()],
            ..Default::default()
        })
        .unwrap();
        let socket = || {
            tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; 2048]),
                tcp::SocketBuffer::new(vec![0; 2048]),
            )
        };
        let mut sockets = SocketSet::new(vec![]);
        let initial = sockets.add(socket());
        let protected = sockets.add(socket());
        let begin_protected = Arc::new(AtomicBool::new(false));
        let stage = Arc::clone(&begin_protected);
        let (wake_tx, wake) = watch::channel(0_u64);
        let (control, runtime) = SecurityNetworkRuntime::new(
            {
                let mut initial_started = false;
                let mut protected_started = false;
                move |interface: &mut Interface, sockets: &mut SocketSet<'static>| {
                    if !initial_started {
                        sockets
                            .get_mut::<tcp::Socket>(initial)
                            .connect(
                                interface.context(),
                                (
                                    "10.0.0.9".parse::<smoltcp::wire::IpAddress>().unwrap(),
                                    5060,
                                ),
                                5050,
                            )
                            .unwrap();
                        initial_started = true;
                    }
                    if stage.load(Ordering::Acquire) && !protected_started {
                        sockets
                            .get_mut::<tcp::Socket>(protected)
                            .connect(
                                interface.context(),
                                (
                                    "10.0.0.9".parse::<smoltcp::wire::IpAddress>().unwrap(),
                                    6060,
                                ),
                                5064,
                            )
                            .unwrap();
                        protected_started = true;
                    }
                    Ok::<_, Infallible>(())
                }
            },
            wake_tx.clone(),
        );
        let (outbound, mut packets) = mpsc::channel(8);
        let (inbound_guard, inbound) = mpsc::channel(1);
        let (stop_tx, stop) = watch::channel(false);

        let runner = network.run_managed(sockets, inbound, outbound, stop, wake, runtime);
        let controller = async {
            let initial = tokio::time::timeout(Duration::from_secs(1), packets.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(initial[9], 6, "initial REGISTER flow must be plaintext TCP");

            let lease = control.install(policy()).await.unwrap();
            begin_protected.store(true, Ordering::Release);
            wake_tx.send_modify(|revision| *revision = revision.wrapping_add(1));

            let protected = loop {
                let packet = tokio::time::timeout(Duration::from_secs(1), packets.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if packet[9] == 50 {
                    break packet;
                }
                if packet[9] == 6 {
                    let source = u16::from_be_bytes([packet[20], packet[21]]);
                    assert_ne!(source, 5064, "protected flow leaked as plaintext TCP");
                }
            };
            assert_eq!(protected[9], 50);
            lease.clear().await.unwrap();
            stop_tx.send(true).unwrap();
            drop(inbound_guard);
        };
        let (result, ()) = tokio::join!(runner, controller);
        result.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dynamic_tcp_registry_arms_port_s_before_protected_port_c() {
        let network = TunnelNetwork::new(&TunnelConfiguration {
            ipv4_addresses: vec!["10.0.0.2".parse().unwrap()],
            ..Default::default()
        })
        .unwrap();
        let (wake_tx, wake) = watch::channel(0_u64);
        let (tcp_control, mut tcp_runtime) = TcpSocketRegistry::new(wake_tx.clone());
        let (security_control, runtime) = SecurityNetworkRuntime::new(
            move |interface: &mut Interface, sockets: &mut SocketSet<'static>| {
                tcp_runtime.service(interface, sockets);
                Ok::<_, Infallible>(())
            },
            wake_tx,
        );
        let (outbound, mut packets) = mpsc::channel(16);
        let (inbound_guard, inbound) = mpsc::channel(1);
        let (stop_tx, stop) = watch::channel(false);
        let runner = network.run_managed(
            SocketSet::new(vec![]),
            inbound,
            outbound,
            stop,
            wake,
            runtime,
        );

        let controller = async {
            let initial = tcp_control
                .dial("10.0.0.9:5060".parse().unwrap(), 5050)
                .await
                .unwrap();
            let packet = tokio::time::timeout(Duration::from_secs(1), packets.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(packet[9], 6, "initial REGISTER SYN must be plaintext TCP");
            assert_eq!(u16::from_be_bytes([packet[20], packet[21]]), 5050);
            assert_eq!(u16::from_be_bytes([packet[22], packet[23]]), 5060);
            initial.close().await.unwrap();

            let security = security_control.install(policy()).await.unwrap();
            while packets.try_recv().is_ok() {}

            // Listener command completion is the ordering barrier used by the
            // next-stage REGISTER coordinator. No protected port-c SYN is
            // permitted until the local port-s socket is actually listening.
            let listener = tcp_control
                .listen(5063, Some("10.0.0.9:6050".parse().unwrap()))
                .await
                .unwrap();
            let protected = tcp_control
                .dial("10.0.0.9:6060".parse().unwrap(), 5064)
                .await
                .unwrap();
            let mut peer_receiver = Receiver::new(
                222,
                Zeroizing::new([1; 16]),
                Zeroizing::new([2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 0, 0, 0, 0]),
            )
            .unwrap();
            loop {
                let packet = tokio::time::timeout(Duration::from_secs(1), packets.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if packet[9] == 50 {
                    let header = usize::from(packet[0] & 0x0f) * 4;
                    let tcp = peer_receiver.open(&packet[header..]).unwrap();
                    assert_eq!(u16::from_be_bytes([tcp[0], tcp[1]]), 5064);
                    assert_eq!(u16::from_be_bytes([tcp[2], tcp[3]]), 6060);
                    assert_ne!(tcp[13] & 0x02, 0, "protected first packet must be SYN");
                    break;
                }
                if packet[9] == 6 {
                    let source = u16::from_be_bytes([packet[20], packet[21]]);
                    assert_ne!(source, 5064, "protected port-c leaked as plaintext TCP");
                }
            }

            protected.close().await.unwrap();
            listener.close().await.unwrap();
            security.clear().await.unwrap();
            stop_tx.send(true).unwrap();
            drop(inbound_guard);
        };
        let (result, ()) = tokio::join!(runner, controller);
        result.unwrap();
    }

    #[test]
    fn cancelled_install_waiter_rolls_back_without_orphaned_sa() {
        let (wake, _receiver) = watch::channel(0_u64);
        let (control, mut runtime) = SecurityNetworkRuntime::new(
            |_: &mut Interface, _: &mut SocketSet<'static>| Ok::<_, Infallible>(()),
            wake,
        );
        let (reply, waiter) = oneshot::channel();
        drop(waiter);
        assert!(
            control
                .commands
                .try_send(SecurityCommand::Install {
                    policy: policy(),
                    reply,
                })
                .is_ok()
        );
        runtime.apply_commands();
        assert_eq!(runtime.slot.installed_generation(), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_fused_registration_install_clears_exact_generation_before_next_boundary() {
        let ready = ready_sec_agree().await;
        let (wake, _receiver) = watch::channel(0_u64);
        let (control, mut runtime) = SecurityNetworkRuntime::new(
            |_: &mut Interface, _: &mut SocketSet<'static>| Ok::<_, Infallible>(()),
            wake,
        );
        let (reply, waiter) = oneshot::channel();
        drop(waiter);
        assert!(
            control
                .commands
                .try_send(SecurityCommand::InstallRegistration {
                    ready: Box::new(ready),
                    reply,
                })
                .is_ok()
        );

        runtime.apply_commands();

        assert_eq!(runtime.slot.installed_generation(), None);
    }

    #[test]
    fn dropping_delivered_lease_queues_exact_cleanup() {
        let (wake, _receiver) = watch::channel(0_u64);
        let (control, mut runtime) = SecurityNetworkRuntime::new(
            |_: &mut Interface, _: &mut SocketSet<'static>| Ok::<_, Infallible>(()),
            wake,
        );
        let (reply, mut waiter) = oneshot::channel();
        assert!(
            control
                .commands
                .try_send(SecurityCommand::Install {
                    policy: policy(),
                    reply,
                })
                .is_ok()
        );
        runtime.apply_commands();
        let lease = waiter.try_recv().unwrap().unwrap();
        assert_eq!(
            runtime.slot.installed_generation(),
            Some(lease.generation())
        );
        drop(lease);
        runtime.apply_commands();
        assert_eq!(runtime.slot.installed_generation(), None);
    }
}
