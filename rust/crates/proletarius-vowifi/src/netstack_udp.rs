//! Bounded UDP sockets on the same userspace IP stack as registered IMS TCP.
//! Datagram submission and exact socket removal are acknowledged by the sole
//! stack owner. RTP never bypasses the authenticated tunnel through host UDP.
use smoltcp::{
    iface::{Interface, SocketHandle, SocketSet},
    socket::udp,
    wire::{IpEndpoint, IpListenEndpoint},
};
use std::{
    collections::BTreeMap,
    io,
    net::{IpAddr, SocketAddr},
};
use tokio::sync::{mpsc, oneshot, watch};
use zeroize::Zeroizing;
const CAPACITY: usize = 8;
const QUEUE: usize = 32;
const MAX_DATAGRAM: usize = 2048;
struct Bind {
    local: IpAddr,
    reply: oneshot::Sender<io::Result<PacketSocket>>,
}
struct Write {
    packet: Zeroizing<Vec<u8>>,
    peer: SocketAddr,
    reply: oneshot::Sender<io::Result<()>>,
}
pub struct Datagram {
    pub bytes: Zeroizing<Vec<u8>>,
    pub peer: SocketAddr,
}
#[derive(Clone)]
pub struct UdpControl {
    commands: mpsc::Sender<Bind>,
    wake: watch::Sender<u64>,
}
impl UdpControl {
    pub fn same_registry(&self, other: &Self) -> bool {
        self.commands.same_channel(&other.commands)
    }
    pub async fn bind(&self, local: IpAddr) -> io::Result<PacketSocket> {
        if local.is_unspecified() || local.is_multicast() {
            return Err(invalid());
        }
        let (reply, result) = oneshot::channel();
        self.commands
            .send(Bind { local, reply })
            .await
            .map_err(|_| closed())?;
        notify(&self.wake);
        result.await.map_err(|_| closed())?
    }
}
#[derive(Clone)]
pub struct PacketSender {
    writes: mpsc::Sender<Write>,
    closing: watch::Sender<bool>,
    wake: watch::Sender<u64>,
    local: SocketAddr,
}
impl PacketSender {
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }
    pub async fn send_to(&self, bytes: Vec<u8>, peer: SocketAddr) -> io::Result<()> {
        if bytes.len() > MAX_DATAGRAM
            || peer.port() == 0
            || peer.ip().is_unspecified()
            || peer.ip().is_multicast()
            || peer.is_ipv4() != self.local.is_ipv4()
        {
            return Err(invalid());
        }
        if *self.closing.borrow() {
            return Err(closed());
        }
        let (reply, result) = oneshot::channel();
        self.writes
            .send(Write {
                packet: Zeroizing::new(bytes),
                peer,
                reply,
            })
            .await
            .map_err(|_| closed())?;
        notify(&self.wake);
        result.await.map_err(|_| closed())?
    }
}
pub struct PacketSocket {
    sender: PacketSender,
    incoming: mpsc::Receiver<Datagram>,
    closed: watch::Receiver<bool>,
}
impl PacketSocket {
    pub fn local_addr(&self) -> SocketAddr {
        self.sender.local
    }
    pub fn sender(&self) -> PacketSender {
        self.sender.clone()
    }
    pub async fn recv(&mut self) -> io::Result<Datagram> {
        let result = self.incoming.recv().await.ok_or_else(closed);
        notify(&self.sender.wake);
        result
    }
    pub async fn close(&mut self) -> io::Result<()> {
        self.sender.closing.send_replace(true);
        notify(&self.sender.wake);
        while !*self.closed.borrow() {
            self.closed.changed().await.map_err(|_| closed())?;
        }
        Ok(())
    }
}
impl Drop for PacketSocket {
    fn drop(&mut self) {
        self.sender.closing.send_replace(true);
        notify(&self.sender.wake);
    }
}
struct Entry {
    handle: SocketHandle,
    local: SocketAddr,
    writes: mpsc::Receiver<Write>,
    pending: Option<Write>,
    incoming: mpsc::Sender<Datagram>,
    closing: watch::Receiver<bool>,
    closed: watch::Sender<bool>,
}
pub struct UdpSocketRegistry {
    commands: mpsc::Receiver<Bind>,
    wake: watch::Sender<u64>,
    entries: BTreeMap<u64, Entry>,
    next: u64,
    next_port: u16,
}
impl UdpSocketRegistry {
    pub fn new(wake: watch::Sender<u64>) -> (UdpControl, Self) {
        let (commands, input) = mpsc::channel(CAPACITY);
        (
            UdpControl {
                commands,
                wake: wake.clone(),
            },
            Self {
                commands: input,
                wake,
                entries: BTreeMap::new(),
                next: 0,
                next_port: 49152,
            },
        )
    }
    pub fn service(&mut self, interface: &mut Interface, sockets: &mut SocketSet<'static>) {
        let removals: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(id, e)| (*e.closing.borrow() || e.incoming.is_closed()).then_some(*id))
            .collect();
        for id in removals {
            let mut entry = self.entries.remove(&id).unwrap();
            sockets.remove(entry.handle);
            entry.writes.close();
            if let Some(write) = entry.pending.take() {
                let _ = write.reply.send(Err(closed()));
            }
            while let Ok(write) = entry.writes.try_recv() {
                let _ = write.reply.send(Err(closed()));
            }
            entry.closed.send_replace(true);
        }
        for _ in 0..CAPACITY {
            let Ok(bind) = self.commands.try_recv() else {
                break;
            };
            if bind.reply.is_closed() {
                continue;
            }
            let result = self.bind(interface, sockets, bind.local);
            if let Err(Ok(socket)) = bind.reply.send(result) {
                drop(socket);
            }
        }
        for entry in self.entries.values_mut() {
            let socket = sockets.get_mut::<udp::Socket>(entry.handle);
            for _ in 0..QUEUE {
                if entry.pending.is_none() {
                    entry.pending = entry.writes.try_recv().ok();
                }
                if !socket.can_send() {
                    break;
                }
                let Some(write) = entry.pending.take() else {
                    break;
                };
                let result = socket
                    .send_slice(
                        &write.packet,
                        IpEndpoint::new(write.peer.ip().into(), write.peer.port()),
                    )
                    .map_err(|_| io::Error::other("native UDP queue rejected packet"));
                let _ = write.reply.send(result);
            }
            for _ in 0..QUEUE {
                if !socket.can_recv() {
                    break;
                }
                let Ok(slot) = entry.incoming.try_reserve() else {
                    break;
                };
                match socket.recv() {
                    Ok((data, metadata)) => {
                        slot.send(Datagram {
                            bytes: Zeroizing::new(data.to_vec()),
                            peer: SocketAddr::new(
                                metadata.endpoint.addr.into(),
                                metadata.endpoint.port,
                            ),
                        });
                    }
                    Err(_) => break,
                }
            }
        }
    }
    pub fn retire(&mut self, sockets: &mut SocketSet<'static>) {
        self.commands.close();
        while let Ok(bind) = self.commands.try_recv() {
            let _ = bind.reply.send(Err(closed()));
        }
        let entries = std::mem::take(&mut self.entries);
        for (_, mut entry) in entries {
            sockets.remove(entry.handle);
            entry.writes.close();
            if let Some(write) = entry.pending.take() {
                let _ = write.reply.send(Err(closed()));
            }
            while let Ok(write) = entry.writes.try_recv() {
                let _ = write.reply.send(Err(closed()));
            }
            entry.closed.send_replace(true);
        }
    }
    fn bind(
        &mut self,
        interface: &mut Interface,
        sockets: &mut SocketSet<'static>,
        local: IpAddr,
    ) -> io::Result<PacketSocket> {
        if self.entries.len() >= CAPACITY {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "native UDP socket capacity reached",
            ));
        }
        if !interface
            .ip_addrs()
            .iter()
            .any(|addr| addr.address() == local.into())
        {
            return Err(invalid());
        }
        let id = self.next.checked_add(1).ok_or_else(invalid)?;
        self.next = id;
        let mut port = None;
        for _ in 49152u32..=65535 {
            let candidate = self.next_port;
            self.next_port = if candidate == 65535 {
                49152
            } else {
                candidate + 1
            };
            if !self.entries.values().any(|e| e.local.port() == candidate) {
                port = Some(candidate);
                break;
            }
        }
        let port = port.ok_or_else(invalid)?;
        let mut socket = udp::Socket::new(
            udp::PacketBuffer::new(
                vec![udp::PacketMetadata::EMPTY; QUEUE],
                vec![0; QUEUE * MAX_DATAGRAM],
            ),
            udp::PacketBuffer::new(
                vec![udp::PacketMetadata::EMPTY; QUEUE],
                vec![0; QUEUE * MAX_DATAGRAM],
            ),
        );
        socket
            .bind(IpListenEndpoint {
                addr: Some(local.into()),
                port,
            })
            .map_err(|_| invalid())?;
        let handle = sockets.add(socket);
        let (writes, write_rx) = mpsc::channel(QUEUE);
        let (incoming, read_rx) = mpsc::channel(QUEUE);
        let (closing, close_rx) = watch::channel(false);
        let (closed, closed_rx) = watch::channel(false);
        let local = SocketAddr::new(local, port);
        self.entries.insert(
            id,
            Entry {
                handle,
                local,
                writes: write_rx,
                pending: None,
                incoming,
                closing: close_rx,
                closed,
            },
        );
        Ok(PacketSocket {
            sender: PacketSender {
                writes,
                closing,
                wake: self.wake.clone(),
                local,
            },
            incoming: read_rx,
            closed: closed_rx,
        })
    }
}
impl crate::netstack_runtime::NetworkRuntime for UdpSocketRegistry {
    type ApplicationError = std::convert::Infallible;
    fn service(
        &mut self,
        interface: &mut Interface,
        sockets: &mut SocketSet<'static>,
    ) -> Result<(), Self::ApplicationError> {
        self.service(interface, sockets);
        Ok(())
    }
    fn retire(&mut self, sockets: &mut SocketSet<'static>) {
        self.retire(sockets);
    }
    fn transform_packet(
        &mut self,
        _: crate::netstack_runtime::PacketDirection,
        packet: Zeroizing<Vec<u8>>,
    ) -> io::Result<crate::netstack_runtime::PacketTransformOutcome> {
        Ok(crate::netstack_runtime::PacketTransformOutcome::Forward(
            packet,
        ))
    }
}

fn notify(wake: &watch::Sender<u64>) {
    wake.send_modify(|v| *v = v.wrapping_add(1));
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid native UDP endpoint or payload",
    )
}
fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "native UDP owner retired")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TunnelConfiguration, netstack_runtime::TunnelNetwork};
    #[tokio::test]
    async fn udp_uses_native_interface_and_close_ack_retires_exact_socket() {
        let config = |ip: &str| TunnelConfiguration {
            ipv4_addresses: vec![ip.parse().unwrap()],
            ..Default::default()
        };
        let a = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
        let b = TunnelNetwork::new(&config("10.0.0.3")).unwrap();
        let (ab_tx, ab_rx) = mpsc::channel(32);
        let (ba_tx, ba_rx) = mpsc::channel(32);
        let (stop, stop_rx) = watch::channel(false);
        let (wa, ra) = watch::channel(0);
        let (wb, rb) = watch::channel(0);
        let (ca, mut sa) = UdpSocketRegistry::new(wa);
        let (cb, mut sb) = UdpSocketRegistry::new(wb);
        let client = tokio::spawn(a.run(
            SocketSet::new(vec![]),
            ba_rx,
            ab_tx,
            stop_rx.clone(),
            ra,
            move |interface, sockets| {
                sa.service(interface, sockets);
                Ok::<_, ()>(())
            },
        ));
        let server = tokio::spawn(b.run(
            SocketSet::new(vec![]),
            ab_rx,
            ba_tx,
            stop_rx,
            rb,
            move |interface, sockets| {
                sb.service(interface, sockets);
                Ok::<_, ()>(())
            },
        ));
        assert!(
            ca.bind("192.0.2.50".parse().unwrap()).await.is_err(),
            "unassigned address cannot bypass tunnel selection"
        );
        let mut a = ca.bind("10.0.0.2".parse().unwrap()).await.unwrap();
        let mut b = cb.bind("10.0.0.3".parse().unwrap()).await.unwrap();
        a.sender()
            .send_to(b"RTP-native".to_vec(), b.local_addr())
            .await
            .unwrap();
        let packet = tokio::time::timeout(std::time::Duration::from_secs(2), b.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(packet.bytes.as_slice(), b"RTP-native");
        assert_eq!(packet.peer, a.local_addr());
        b.sender()
            .send_to(b"reverse".to_vec(), a.local_addr())
            .await
            .unwrap();
        assert_eq!(a.recv().await.unwrap().bytes.as_slice(), b"reverse");
        let old = a.sender();
        a.close().await.unwrap();
        assert!(old.send_to(vec![1], b.local_addr()).await.is_err());
        let mut replacement = ca.bind("10.0.0.2".parse().unwrap()).await.unwrap();
        replacement
            .sender()
            .send_to(b"new".to_vec(), b.local_addr())
            .await
            .unwrap();
        assert_eq!(b.recv().await.unwrap().bytes.as_slice(), b"new");
        assert!(old.send_to(vec![1], b.local_addr()).await.is_err());
        replacement.close().await.unwrap();
        b.close().await.unwrap();
        stop.send_replace(true);
        assert!(client.await.unwrap().is_ok());
        assert!(server.await.unwrap().is_ok());
    }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    use crate::{TunnelConfiguration, netstack_runtime::TunnelNetwork};
    #[tokio::test]
    async fn network_abort_removes_socket_before_publishing_close_proof() {
        let config = TunnelConfiguration {
            ipv4_addresses: vec!["10.0.0.2".parse().unwrap()],
            ..Default::default()
        };
        let network = TunnelNetwork::new(&config).unwrap();
        let (wake, rx) = watch::channel(0);
        let (control, registry) = UdpSocketRegistry::new(wake);
        let (_inbound, input) = mpsc::channel(32);
        let (outbound, _output) = mpsc::channel(32);
        let (_stop, stop) = watch::channel(false);
        let task = tokio::spawn(network.run_managed(
            SocketSet::new(vec![]),
            input,
            outbound,
            stop,
            rx,
            registry,
        ));
        let mut socket = control.bind("10.0.0.2".parse().unwrap()).await.unwrap();
        let sender = socket.sender();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        socket.close().await.unwrap();
        assert!(
            sender
                .send_to(vec![1], "10.0.0.3:49170".parse().unwrap())
                .await
                .is_err()
        );
        assert!(control.bind("10.0.0.2".parse().unwrap()).await.is_err());
    }
}
