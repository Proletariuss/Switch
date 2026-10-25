//! Owned userspace IP interface driven only by the native tunnel queues.
use crate::{
    TunnelConfiguration,
    netstack_device::{DeviceError, TunnelIpDevice},
};
use smoltcp::{
    iface::{Config, Interface, SocketSet},
    time::Instant,
    wire::{HardwareAddress, IpCidr},
};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq)]
pub enum NetworkConfigError {
    MissingAddress,
    InvalidAddress,
    AddressCapacity,
    RouteCapacity,
    Entropy,
}
#[derive(Debug)]
pub enum NetworkRunError<E> {
    TunnelClosed,
    SendTimeout,
    Device(DeviceError),
    Application(E),
    SocketCapacity,
    PacketTransform(std::io::Error),
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct NetworkRunStats {
    pub inbound_forwarded: u64,
    pub inbound_discarded: u64,
    pub outbound_forwarded: u64,
    pub max_inbound_wire_bytes: usize,
    pub max_outbound_wire_bytes: usize,
}

#[derive(Clone, Copy)]
pub enum PacketDirection {
    Inbound,
    Outbound,
}

pub enum PacketTransformOutcome {
    Forward(Zeroizing<Vec<u8>>),
    Discard,
    /// A validated zero-payload control packet for an exact retired IMS TCP flow.
    RetiredControlDiscard,
}

/// One owner for application sockets and packet transforms. Implementations
/// may process control commands in `service`, then apply the resulting state
/// in `transform_packet` without a cross-task lock on every packet.
pub trait NetworkRuntime {
    type ApplicationError;

    /// Synchronous retirement in the same stack owner. Implementations remove
    /// exact sockets before publishing their close acknowledgements.
    fn retire(&mut self, _sockets: &mut SocketSet<'static>) {}

    fn service(
        &mut self,
        interface: &mut Interface,
        sockets: &mut SocketSet<'static>,
    ) -> Result<(), Self::ApplicationError>;

    fn transform_packet(
        &mut self,
        direction: PacketDirection,
        packet: Zeroizing<Vec<u8>>,
    ) -> Result<PacketTransformOutcome, std::io::Error>;
}

struct ClosureRuntime<F, P> {
    service: F,
    transform: P,
}

pub(crate) struct IdentityNetworkRuntime<F>(F);

pub(crate) fn identity_runtime<F>(service: F) -> IdentityNetworkRuntime<F> {
    IdentityNetworkRuntime(service)
}

impl<F, E> NetworkRuntime for IdentityNetworkRuntime<F>
where
    F: FnMut(&mut Interface, &mut SocketSet<'static>) -> Result<(), E>,
{
    type ApplicationError = E;

    fn service(
        &mut self,
        interface: &mut Interface,
        sockets: &mut SocketSet<'static>,
    ) -> Result<(), Self::ApplicationError> {
        (self.0)(interface, sockets)
    }

    fn transform_packet(
        &mut self,
        _direction: PacketDirection,
        packet: Zeroizing<Vec<u8>>,
    ) -> Result<PacketTransformOutcome, std::io::Error> {
        Ok(PacketTransformOutcome::Forward(packet))
    }
}

impl<F, E, P> NetworkRuntime for ClosureRuntime<F, P>
where
    F: FnMut(&mut Interface, &mut SocketSet<'static>) -> Result<(), E>,
    P: FnMut(PacketDirection, Zeroizing<Vec<u8>>) -> Result<PacketTransformOutcome, std::io::Error>,
{
    type ApplicationError = E;

    fn service(
        &mut self,
        interface: &mut Interface,
        sockets: &mut SocketSet<'static>,
    ) -> Result<(), Self::ApplicationError> {
        (self.service)(interface, sockets)
    }

    fn transform_packet(
        &mut self,
        direction: PacketDirection,
        packet: Zeroizing<Vec<u8>>,
    ) -> Result<PacketTransformOutcome, std::io::Error> {
        (self.transform)(direction, packet)
    }
}

const WORK_BEFORE_YIELD: u8 = 32;

async fn cooperative_yield(work: &mut u8) {
    *work = work.saturating_add(1);
    if *work >= WORK_BEFORE_YIELD {
        *work = 0;
        tokio::task::yield_now().await;
    }
}

pub struct TunnelNetwork {
    interface: Interface,
    device: TunnelIpDevice,
}

struct RuntimeSockets<R: NetworkRuntime> {
    runtime: R,
    sockets: SocketSet<'static>,
}
impl<R: NetworkRuntime> Drop for RuntimeSockets<R> {
    fn drop(&mut self) {
        self.runtime.retire(&mut self.sockets);
    }
}

async fn cancelled(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow() || stop.changed().await.is_err() {
            return;
        }
    }
}

impl TunnelNetwork {
    /// Configures only this session's IP-medium interface. Default routes use
    /// its local address as a point-to-point next hop, not a host gateway.
    pub fn new(config: &TunnelConfiguration) -> Result<Self, NetworkConfigError> {
        use NetworkConfigError::*;
        if config.ipv4_addresses.is_empty() && config.ipv6_addresses.is_empty() {
            return Err(MissingAddress);
        }
        let mut addresses = Vec::new();
        for ip in &config.ipv4_addresses {
            if ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
                return Err(InvalidAddress);
            }
            let cidr = IpCidr::new((*ip).into(), 32);
            if !addresses.contains(&cidr) {
                addresses.push(cidr);
            }
        }
        for (ip, prefix) in &config.ipv6_addresses {
            if ip.is_unspecified() || ip.is_multicast() || *prefix > 128 {
                return Err(InvalidAddress);
            }
            let cidr = IpCidr::new((*ip).into(), *prefix);
            if !addresses.contains(&cidr) {
                addresses.push(cidr);
            }
        }
        let mut seed = [0; 8];
        getrandom::fill(&mut seed).map_err(|_| Entropy)?;
        let mut settings = Config::new(HardwareAddress::Ip);
        settings.random_seed = u64::from_ne_bytes(seed);
        let mut device = TunnelIpDevice::default();
        let mut interface = Interface::new(settings, &mut device, Instant::ZERO);
        let mut capacity_error = false;
        interface.update_ip_addrs(|ips| {
            for ip in addresses {
                if ips.push(ip).is_err() {
                    capacity_error = true;
                    break;
                }
            }
        });
        if capacity_error {
            return Err(AddressCapacity);
        }
        if let Some(ip) = config.ipv4_addresses.first() {
            interface
                .routes_mut()
                .add_default_ipv4_route(*ip)
                .map_err(|_| RouteCapacity)?;
        }
        if let Some((ip, _)) = config.ipv6_addresses.first() {
            interface
                .routes_mut()
                .add_default_ipv6_route(*ip)
                .map_err(|_| RouteCapacity)?;
        }
        Ok(Self { interface, device })
    }

    /// `service` performs nonblocking socket work and must not access hardware
    /// or OS networking. A caller changes `wake` when application work arrives.
    /// This future owns all sockets; dropping it releases them and IP queues.
    /// Success means requested cancellation, never IMS readiness.
    pub async fn run<F, E>(
        self,
        sockets: SocketSet<'static>,
        inbound: mpsc::Receiver<Zeroizing<Vec<u8>>>,
        outbound: mpsc::Sender<Zeroizing<Vec<u8>>>,
        stop: watch::Receiver<bool>,
        wake: watch::Receiver<u64>,
        service: F,
    ) -> Result<(), NetworkRunError<E>>
    where
        F: FnMut(&mut Interface, &mut SocketSet<'static>) -> Result<(), E>,
    {
        self.run_managed(
            sockets,
            inbound,
            outbound,
            stop,
            wake,
            identity_runtime(service),
        )
        .await
        .map(|_| ())
    }

    /// Transform packets between the TCP/IP stack and tunnel, with the same
    /// owner/lifetime as the sockets. A transform error never bypasses it.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_transformed<F, E, P>(
        self,
        sockets: SocketSet<'static>,
        inbound: mpsc::Receiver<Zeroizing<Vec<u8>>>,
        outbound: mpsc::Sender<Zeroizing<Vec<u8>>>,
        stop: watch::Receiver<bool>,
        wake: watch::Receiver<u64>,
        service: F,
        transform: P,
    ) -> Result<NetworkRunStats, NetworkRunError<E>>
    where
        F: FnMut(&mut Interface, &mut SocketSet<'static>) -> Result<(), E>,
        P: FnMut(
            PacketDirection,
            Zeroizing<Vec<u8>>,
        ) -> Result<PacketTransformOutcome, std::io::Error>,
    {
        self.run_managed(
            sockets,
            inbound,
            outbound,
            stop,
            wake,
            ClosureRuntime { service, transform },
        )
        .await
    }

    /// Runs a single owned socket/transform runtime. Control-command ack and
    /// packet routing therefore share one task and one ordering boundary.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_managed<R>(
        mut self,
        sockets: SocketSet<'static>,
        mut inbound: mpsc::Receiver<Zeroizing<Vec<u8>>>,
        outbound: mpsc::Sender<Zeroizing<Vec<u8>>>,
        mut stop: watch::Receiver<bool>,
        mut wake: watch::Receiver<u64>,
        runtime: R,
    ) -> Result<NetworkRunStats, NetworkRunError<R::ApplicationError>>
    where
        R: NetworkRuntime,
    {
        // This guard runs on normal/error return, panic, or future cancellation.
        // SocketSet stays alive until its registry has removed the exact handles.
        let mut owner = RuntimeSockets { runtime, sockets };
        let (runtime, mut sockets) = (&mut owner.runtime, &mut owner.sockets);
        let epoch = tokio::time::Instant::now();
        let mut stats = NetworkRunStats::default();
        let mut work_since_yield = 0;
        loop {
            if *stop.borrow() || stop.has_changed().is_err() {
                return Ok(stats);
            }
            if sockets.iter().count() > 16 {
                return Err(NetworkRunError::SocketCapacity);
            }
            let now =
                Instant::from_millis(epoch.elapsed().as_millis().min(i64::MAX as u128) as i64);
            runtime
                .service(&mut self.interface, &mut sockets)
                .map_err(NetworkRunError::Application)?;
            if sockets.iter().count() > 16 {
                return Err(NetworkRunError::SocketCapacity);
            }
            self.interface.poll(now, &mut self.device, &mut sockets);
            runtime
                .service(&mut self.interface, &mut sockets)
                .map_err(NetworkRunError::Application)?;
            if sockets.iter().count() > 16 {
                return Err(NetworkRunError::SocketCapacity);
            }
            self.interface.poll(now, &mut self.device, &mut sockets);
            while let Some(packet) = self.device.pop_outbound() {
                let packet = match runtime
                    .transform_packet(PacketDirection::Outbound, packet)
                    .map_err(NetworkRunError::PacketTransform)?
                {
                    PacketTransformOutcome::Forward(packet) => packet,
                    PacketTransformOutcome::RetiredControlDiscard => {
                        cooperative_yield(&mut work_since_yield).await;
                        continue;
                    }
                    PacketTransformOutcome::Discard => {
                        return Err(NetworkRunError::PacketTransform(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "outbound packet transform returned Discard",
                        )));
                    }
                };
                stats.max_outbound_wire_bytes = stats.max_outbound_wire_bytes.max(packet.len());
                tokio::select! {
                    biased;
                    _ = cancelled(&mut stop) => return Ok(stats),
                    sent = tokio::time::timeout(Duration::from_secs(5), outbound.send(packet)) => {
                        sent.map_err(|_| NetworkRunError::SendTimeout)?.map_err(|_| NetworkRunError::TunnelClosed)?;
                        stats.outbound_forwarded = stats.outbound_forwarded.saturating_add(1);
                    }
                }
                cooperative_yield(&mut work_since_yield).await;
            }
            let delay = self
                .interface
                .poll_delay(now, &sockets)
                .map(|delay| Duration::from_millis(delay.total_millis()))
                .unwrap_or(Duration::from_secs(1))
                .clamp(Duration::from_millis(1), Duration::from_secs(1));
            tokio::select! {
                biased;
                _ = cancelled(&mut stop) => return Ok(stats),
                _ = outbound.closed() => return Err(NetworkRunError::TunnelClosed),
                packet = inbound.recv() => {
                    let packet = packet.ok_or(NetworkRunError::TunnelClosed)?;
                    stats.max_inbound_wire_bytes = stats.max_inbound_wire_bytes.max(packet.len());
                    match runtime.transform_packet(PacketDirection::Inbound, packet).map_err(NetworkRunError::PacketTransform)? {
                        PacketTransformOutcome::Forward(packet) => match self.device.push_inbound(packet) {
                            Ok(()) => stats.inbound_forwarded = stats.inbound_forwarded.saturating_add(1),
                            Err(DeviceError::InvalidPacketLength) => {
                                stats.inbound_discarded = stats.inbound_discarded.saturating_add(1);
                            }
                            Err(error) => return Err(NetworkRunError::Device(error)),
                        },
                        PacketTransformOutcome::RetiredControlDiscard=>return Err(NetworkRunError::PacketTransform(std::io::Error::new(std::io::ErrorKind::InvalidData,"retired TCP control discard is outbound-only"))),
                        PacketTransformOutcome::Discard => {
                            stats.inbound_discarded = stats.inbound_discarded.saturating_add(1);
                        }
                    }
                    cooperative_yield(&mut work_since_yield).await;
                }
                changed = wake.changed() => {
                    if changed.is_err() { return Err(NetworkRunError::TunnelClosed); }
                    cooperative_yield(&mut work_since_yield).await;
                }
                _ = tokio::time::sleep(delay) => { work_since_yield = 0; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::socket::{tcp, udp};
    use std::{cell::Cell, rc::Rc};

    fn configuration(ip: &str) -> TunnelConfiguration {
        TunnelConfiguration {
            ipv4_addresses: vec![ip.parse().unwrap()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn assigned_host_routes_exchange_tcp_udp_and_cancel() {
        let a = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let b = TunnelNetwork::new(&configuration("10.20.30.9")).unwrap();
        let (ab_tx, ab_rx) = mpsc::channel(4);
        let (ba_tx, ba_rx) = mpsc::channel(4);
        let ab_monitor = ab_tx.clone();
        let ba_monitor = ba_tx.clone();
        let (stop_tx, stop) = watch::channel(false);
        let (_wake_tx, wake) = watch::channel(0);
        let socket = || {
            let mut socket = udp::Socket::new(
                udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 4096]),
                udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 4096]),
            );
            socket.bind(5060).unwrap();
            socket
        };
        let mut sa = SocketSet::new(vec![]);
        let ha = sa.add(socket());
        let mut sb = SocketSet::new(vec![]);
        let hb = sb.add(socket());
        let tcp_socket = || {
            tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; 4096]),
                tcp::SocketBuffer::new(vec![0; 4096]),
            )
        };
        let ta = sa.add(tcp_socket());
        let tb = sb.add(tcp_socket());
        sb.get_mut::<tcp::Socket>(tb).listen(5060).unwrap();
        let (mut connected, mut tcp_sent, mut tcp_received) = (false, false, false);
        let mut sent = false;
        let mut received = false;
        let client = a.run(
            sa,
            ba_rx,
            ab_tx,
            stop.clone(),
            wake.clone(),
            |interface, sockets| {
                let stream = sockets.get_mut::<tcp::Socket>(ta);
                if !connected {
                    stream
                        .connect(
                            interface.context(),
                            (
                                "10.20.30.9".parse::<smoltcp::wire::IpAddress>().unwrap(),
                                5060,
                            ),
                            5060,
                        )
                        .unwrap();
                    connected = true;
                }
                if stream.can_send() && !tcp_sent {
                    stream.send_slice(b"REGISTER").unwrap();
                    tcp_sent = true;
                }
                if stream.can_recv() {
                    let mut buf = [0; 64];
                    let n = stream.recv_slice(&mut buf).unwrap();
                    assert_eq!(&buf[..n], b"200 OK");
                    tcp_received = true;
                }
                let socket = sockets.get_mut::<udp::Socket>(ha);
                if !sent {
                    socket
                        .send_slice(
                            b"REGISTER",
                            (
                                "10.20.30.9".parse::<smoltcp::wire::IpAddress>().unwrap(),
                                5060,
                            ),
                        )
                        .unwrap();
                    sent = true;
                }
                if socket.can_recv() {
                    let (data, _) = socket.recv().unwrap();
                    assert_eq!(data, b"200 OK");
                    received = true;
                }
                if received && tcp_received {
                    stop_tx.send(true).unwrap();
                }
                Ok::<_, ()>(())
            },
        );
        let server = b.run(sb, ab_rx, ba_tx, stop, wake, |_, sockets| {
            let stream = sockets.get_mut::<tcp::Socket>(tb);
            if stream.can_recv() {
                let mut buf = [0; 64];
                let n = stream.recv_slice(&mut buf).unwrap();
                assert_eq!(&buf[..n], b"REGISTER");
                stream.send_slice(b"200 OK").unwrap();
            }
            let socket = sockets.get_mut::<udp::Socket>(hb);
            if socket.can_recv() {
                let (data, meta) = socket.recv().unwrap();
                assert_eq!(data, b"REGISTER");
                socket.send_slice(b"200 OK", meta.endpoint).unwrap();
            }
            Ok::<_, ()>(())
        });
        let (a, b) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(client, server)
        })
        .await
        .unwrap();
        a.unwrap();
        b.unwrap();
        assert!(received && tcp_received);
        assert!(ab_monitor.is_closed() && ba_monitor.is_closed());
    }

    #[tokio::test]
    async fn closed_tunnel_is_an_error_not_online() {
        let network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        let (out, _rx) = mpsc::channel(1);
        let (_stop, stop) = watch::channel(false);
        let (_wake, wake) = watch::channel(0);
        let result = network
            .run(SocketSet::new(vec![]), rx, out, stop, wake, |_, _| {
                Ok::<_, ()>(())
            })
            .await;
        assert!(matches!(result, Err(NetworkRunError::TunnelClosed)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sustained_ready_input_yields_to_stop_at_work_budget() {
        let network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let work_budget = usize::from(WORK_BEFORE_YIELD);
        // The extra item is essential: without the explicit cooperative yield,
        // input must remain ready rather than naturally pending at the budget.
        let ready_items = work_budget + 1;
        let (inbound_tx, inbound) = mpsc::channel(ready_items);
        for _ in 0..ready_items {
            inbound_tx.try_send(Zeroizing::new(vec![0xaa; 37])).unwrap();
        }
        let (outbound, _outbound_rx) = mpsc::channel(1);
        let (stop_tx, stop) = watch::channel(false);
        let (_wake_tx, wake) = watch::channel(0);
        let handled = Rc::new(Cell::new(0));
        let transform_handled = Rc::clone(&handled);
        // Disable Tokio's channel-level cooperative budget so the first
        // Pending is specifically the runtime's WORK_BEFORE_YIELD boundary.
        let runner = tokio::task::unconstrained(network.run_transformed(
            SocketSet::new(vec![]),
            inbound,
            outbound,
            stop,
            wake,
            |_, _| Ok::<_, ()>(()),
            move |direction, packet| match direction {
                PacketDirection::Inbound => {
                    transform_handled.set(transform_handled.get() + 1);
                    Ok(PacketTransformOutcome::Discard)
                }
                PacketDirection::Outbound => Ok(PacketTransformOutcome::Forward(packet)),
            },
        ));
        let stopper = async {
            // `biased` join polls `runner` first. Reaching this future proves
            // the runner returned Pending while another input remained ready.
            assert_eq!(handled.get(), work_budget);
            stop_tx.send(true).unwrap();
        };
        let (result, ()) = tokio::join!(biased; runner, stopper);
        let stats = result.unwrap();
        assert_eq!(handled.get(), work_budget);
        assert_eq!(stats.inbound_discarded, u64::from(WORK_BEFORE_YIELD));
        assert_eq!(stats.inbound_forwarded, 0);
        drop(inbound_tx);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wake_storm_cannot_starve_ready_tunnel_input() {
        let network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let (inbound_tx, inbound) = mpsc::channel(1);
        inbound_tx.try_send(Zeroizing::new(vec![0xaa; 37])).unwrap();
        let (outbound, _outbound_rx) = mpsc::channel(1);
        let (stop_tx, stop) = watch::channel(false);
        let (wake_tx, wake) = watch::channel(0_u64);
        let wake_storm = wake_tx.clone();
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            network.run_transformed(
                SocketSet::new(vec![]),
                inbound,
                outbound,
                stop,
                wake,
                move |_, _| {
                    wake_storm.send_modify(|revision| *revision = revision.wrapping_add(1));
                    Ok::<_, ()>(())
                },
                move |direction, packet| match direction {
                    PacketDirection::Inbound => {
                        stop_tx.send(true).unwrap();
                        Ok(PacketTransformOutcome::Discard)
                    }
                    PacketDirection::Outbound => Ok(PacketTransformOutcome::Forward(packet)),
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.inbound_discarded, 1);
        drop(inbound_tx);
        drop(wake_tx);
    }

    #[tokio::test]
    async fn inbound_discard_is_counted_without_ending_network() {
        let network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let (inbound_tx, inbound) = mpsc::channel(1);
        inbound_tx.try_send(Zeroizing::new(vec![0xaa; 37])).unwrap();
        let (outbound, _outbound_rx) = mpsc::channel(1);
        let (stop_tx, stop) = watch::channel(false);
        let (_wake_tx, wake) = watch::channel(0);
        let stats = tokio::time::timeout(
            Duration::from_millis(100),
            network.run_transformed(
                SocketSet::new(vec![]),
                inbound,
                outbound,
                stop,
                wake,
                |_, _| Ok::<_, ()>(()),
                move |direction, packet| match direction {
                    PacketDirection::Inbound => {
                        stop_tx.send(true).unwrap();
                        Ok(PacketTransformOutcome::Discard)
                    }
                    PacketDirection::Outbound => Ok(PacketTransformOutcome::Forward(packet)),
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(stats.inbound_discarded, 1);
        assert_eq!(stats.inbound_forwarded, 0);
        assert_eq!(stats.max_inbound_wire_bytes, 37);
    }

    #[tokio::test]
    async fn outbound_discard_fails_closed_before_tunnel_send() {
        let network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let (inbound_tx, inbound) = mpsc::channel(1);
        let (outbound, mut outbound_rx) = mpsc::channel(1);
        let outbound_monitor = outbound.clone();
        let (_stop_tx, stop) = watch::channel(false);
        let (_wake_tx, wake) = watch::channel(0);
        let mut sockets = SocketSet::new(vec![]);
        let mut socket = udp::Socket::new(
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 1], vec![0; 128]),
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 1], vec![0; 128]),
        );
        socket.bind(5060).unwrap();
        let handle = sockets.add(socket);
        let mut queued = false;
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            network.run_transformed(
                sockets,
                inbound,
                outbound,
                stop,
                wake,
                move |_, sockets| {
                    if !queued {
                        sockets
                            .get_mut::<udp::Socket>(handle)
                            .send_slice(
                                b"REGISTER",
                                (
                                    "10.20.30.9".parse::<smoltcp::wire::IpAddress>().unwrap(),
                                    5060,
                                ),
                            )
                            .unwrap();
                        queued = true;
                    }
                    Ok::<_, ()>(())
                },
                |direction, packet| match direction {
                    PacketDirection::Outbound => Ok(PacketTransformOutcome::Discard),
                    PacketDirection::Inbound => Ok(PacketTransformOutcome::Forward(packet)),
                },
            ),
        )
        .await
        .unwrap();
        drop(inbound_tx);
        assert!(matches!(
            result,
            Err(NetworkRunError::PacketTransform(error))
                if error.kind() == std::io::ErrorKind::InvalidData
        ));
        assert!(matches!(
            outbound_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(outbound_monitor);
    }
    #[tokio::test]
    async fn retired_control_discard_is_not_an_inbound_validation_bypass() {
        let network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let (input, inbound) = mpsc::channel(1);
        let (outbound, _output) = mpsc::channel(1);
        let (_stop, stop) = watch::channel(false);
        let (_wake, wake) = watch::channel(0);
        input.send(Zeroizing::new(vec![0; 40])).await.unwrap();
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            network.run_transformed(
                SocketSet::new(vec![]),
                inbound,
                outbound,
                stop,
                wake,
                |_, _| Ok::<_, ()>(()),
                |direction, packet| match direction {
                    PacketDirection::Inbound => Ok(PacketTransformOutcome::RetiredControlDiscard),
                    PacketDirection::Outbound => Ok(PacketTransformOutcome::Forward(packet)),
                },
            ),
        )
        .await
        .unwrap();
        assert!(
            matches!(result,Err(NetworkRunError::PacketTransform(error)) if error.kind()==std::io::ErrorKind::InvalidData)
        );
    }
}
