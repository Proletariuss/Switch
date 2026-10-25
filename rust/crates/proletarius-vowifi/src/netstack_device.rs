//! Per-tunnel IP medium for the userspace stack. No OS socket, DNS or route.
//! The runtime drains outbound packets into NativeTunnel and feeds decrypted
//! inbound packets here. Neither a device nor its queues may be shared by SIMs.
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use std::collections::VecDeque;
use zeroize::Zeroizing;

pub const IP_MTU: usize = 1280;
pub const QUEUE_PACKETS: usize = 64;

#[derive(Debug, Eq, PartialEq)]
pub enum DeviceError {
    Closed,
    QueueFull,
    InvalidPacketLength,
}

#[derive(Default)]
pub struct TunnelIpDevice {
    inbound: VecDeque<Zeroizing<Vec<u8>>>,
    outbound: VecDeque<Zeroizing<Vec<u8>>>,
    closed: bool,
}

impl TunnelIpDevice {
    /// Input is already ESP-authenticated and traffic-selector checked. The
    /// stack still checks IP/transport headers and checksums (no offloading).
    pub fn push_inbound(&mut self, packet: Zeroizing<Vec<u8>>) -> Result<(), DeviceError> {
        if self.closed {
            return Err(DeviceError::Closed);
        }
        if packet.len() < 20 || packet.len() > IP_MTU {
            return Err(DeviceError::InvalidPacketLength);
        }
        if self.inbound.len() == QUEUE_PACKETS {
            return Err(DeviceError::QueueFull);
        }
        self.inbound.push_back(packet);
        Ok(())
    }

    pub fn pop_outbound(&mut self) -> Option<Zeroizing<Vec<u8>>> {
        self.outbound.pop_front()
    }

    /// The runtime must also drop its SocketSet to release stream buffers.
    pub fn close(&mut self) {
        self.closed = true;
        self.inbound.clear();
        self.outbound.clear();
    }
}

impl Device for TunnelIpDevice {
    type RxToken<'a> = RxPacket;
    type TxToken<'a> = TxPacket<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ip;
        capabilities.max_transmission_unit = IP_MTU;
        capabilities.max_burst_size = Some(QUEUE_PACKETS);
        capabilities
    }

    fn receive(&mut self, _: Instant) -> Option<(RxPacket, TxPacket<'_>)> {
        // A receive token needs room for an immediate ACK/error reply. Keep
        // inbound queued during backpressure rather than silently discarding.
        if self.closed || self.outbound.len() == QUEUE_PACKETS {
            return None;
        }
        let packet = self.inbound.pop_front()?;
        Some((RxPacket(packet), TxPacket(&mut self.outbound)))
    }

    fn transmit(&mut self, _: Instant) -> Option<TxPacket<'_>> {
        if self.closed || self.outbound.len() == QUEUE_PACKETS {
            return None;
        }
        Some(TxPacket(&mut self.outbound))
    }
}

#[doc(hidden)]
pub struct RxPacket(Zeroizing<Vec<u8>>);
impl phy::RxToken for RxPacket {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

#[doc(hidden)]
pub struct TxPacket<'a>(&'a mut VecDeque<Zeroizing<Vec<u8>>>);
impl phy::TxToken for TxPacket<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        // smoltcp sizes IP frames using the advertised MTU.
        let mut packet = Zeroizing::new(vec![0; len]);
        let result = f(&mut packet);
        self.0.push_back(packet);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::phy::TxToken;
    use smoltcp::{
        iface::{Config, Interface, SocketSet},
        socket::{tcp, udp},
        wire::{HardwareAddress, IpAddress, IpCidr},
    };

    fn peer(address: IpAddress) -> (TunnelIpDevice, Interface, SocketSet<'static>) {
        let mut device = TunnelIpDevice::default();
        let mut config = Config::new(HardwareAddress::Ip);
        config.random_seed = 17;
        let mut interface = Interface::new(config, &mut device, Instant::ZERO);
        interface.update_ip_addrs(|addresses| {
            addresses.push(IpCidr::new(address, 24)).unwrap();
        });
        (device, interface, SocketSet::new(vec![]))
    }
    fn move_packets(from: &mut TunnelIpDevice, to: &mut TunnelIpDevice) {
        while let Some(packet) = from.pop_outbound() {
            to.push_inbound(packet).unwrap();
        }
    }
    fn tcp_socket() -> tcp::Socket<'static> {
        tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; 4096]),
            tcp::SocketBuffer::new(vec![0; 4096]),
        )
    }
    fn udp_socket() -> udp::Socket<'static> {
        udp::Socket::new(
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 4096]),
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 4096]),
        )
    }

    /// Independent Internet checksum verification, including the checksum
    /// field. A valid packet folds to all ones.
    fn checksum_valid(parts: &[&[u8]]) -> bool {
        let mut sum = 0_u32;
        for part in parts {
            for pair in part.chunks(2) {
                sum += u32::from(u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]));
            }
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        sum == 0xffff
    }

    #[test]
    fn outbound_ipv4_tcp_syn_has_real_header_and_transport_checksums() {
        // A tunnel cannot rely on NIC checksum offload. This is the actual
        // smoltcp packet taken from the queue that later enters ESP, checked
        // with arithmetic independent of smoltcp's parser or peer stack.
        let local: IpAddress = "10.0.0.2".parse().unwrap();
        let remote: IpAddress = "10.0.0.9".parse().unwrap();
        let (mut device, mut interface, mut sockets) = peer(local);
        let handle = sockets.add(tcp_socket());
        sockets
            .get_mut::<tcp::Socket>(handle)
            .connect(interface.context(), (remote, 5060), 42000)
            .unwrap();
        interface.poll(Instant::ZERO, &mut device, &mut sockets);
        let packet = device.pop_outbound().expect("TCP SYN from IP medium");
        assert_eq!(packet[0] >> 4, 4);
        let header_len = usize::from(packet[0] & 15) * 4;
        assert!(header_len >= 20);
        assert_eq!(
            usize::from(u16::from_be_bytes([packet[2], packet[3]])),
            packet.len()
        );
        assert_eq!(packet[9], 6);
        assert_eq!(&packet[12..16], &[10, 0, 0, 2]);
        assert_eq!(&packet[16..20], &[10, 0, 0, 9]);
        assert!(checksum_valid(&[&packet[..header_len]]));

        let tcp = &packet[header_len..];
        assert!(tcp.len() >= 20);
        assert_eq!(u16::from_be_bytes([tcp[0], tcp[1]]), 42000);
        assert_eq!(u16::from_be_bytes([tcp[2], tcp[3]]), 5060);
        assert_eq!(tcp[13] & 0x12, 0x02); // SYN, without ACK.
        let pseudo = [0, 6, (tcp.len() >> 8) as u8, tcp.len() as u8];
        assert!(checksum_valid(&[&packet[12..20], &pseudo, tcp]));
        let mut damaged = packet.to_vec();
        damaged[header_len + 8] ^= 1;
        assert!(!checksum_valid(&[
            &damaged[12..20],
            &pseudo,
            &damaged[header_len..],
        ]));
    }

    #[test]
    fn outbound_ipv6_tcp_syn_has_real_transport_checksum() {
        // IPv6 has no header checksum, so validate the actual TCP checksum
        // using an independently assembled IPv6 pseudo-header. This packet is
        // the same queue output later passed to ESP by TunnelNetwork.
        let local: IpAddress = "2001:db8::2".parse().unwrap();
        let remote: IpAddress = "2001:db8::9".parse().unwrap();
        let (mut device, mut interface, mut sockets) = peer(local);
        let handle = sockets.add(tcp_socket());
        sockets
            .get_mut::<tcp::Socket>(handle)
            .connect(interface.context(), (remote, 5060), 42000)
            .unwrap();
        interface.poll(Instant::ZERO, &mut device, &mut sockets);
        let packet = device.pop_outbound().expect("IPv6 TCP SYN from IP medium");

        assert_eq!(packet[0] >> 4, 6);
        assert_eq!(
            usize::from(u16::from_be_bytes([packet[4], packet[5]])),
            packet.len() - 40
        );
        assert_eq!(packet[6], 6, "IPv6 next header is TCP");
        let local_octets = "2001:db8::2"
            .parse::<std::net::Ipv6Addr>()
            .unwrap()
            .octets();
        let remote_octets = "2001:db8::9"
            .parse::<std::net::Ipv6Addr>()
            .unwrap()
            .octets();
        assert_eq!(&packet[8..24], &local_octets);
        assert_eq!(&packet[24..40], &remote_octets);

        let tcp = &packet[40..];
        assert!(tcp.len() >= 20);
        assert_eq!(u16::from_be_bytes([tcp[0], tcp[1]]), 42000);
        assert_eq!(u16::from_be_bytes([tcp[2], tcp[3]]), 5060);
        assert_eq!(tcp[13] & 0x12, 0x02); // SYN, without ACK.
        assert_eq!(usize::from(tcp[12] >> 4) * 4, tcp.len());
        let tcp_length = u32::try_from(tcp.len()).unwrap().to_be_bytes();
        let next_header = [0, 0, 0, 6];
        assert!(checksum_valid(&[
            &packet[8..40],
            &tcp_length,
            &next_header,
            tcp,
        ]));
        let mut damaged = packet.to_vec();
        damaged[48] ^= 1;
        assert!(!checksum_valid(&[
            &damaged[8..40],
            &tcp_length,
            &next_header,
            &damaged[40..],
        ]));
    }

    #[test]
    fn tcp_and_udp_roundtrip_over_ip_queues() {
        for (a, b) in [("10.0.0.2", "10.0.0.9"), ("fd00::2", "fd00::9")] {
            roundtrip(a.parse().unwrap(), b.parse().unwrap());
        }
    }

    fn roundtrip(local: IpAddress, remote: IpAddress) {
        let (mut a, mut ai, mut sockets_a) = peer(local);
        let (mut b, mut bi, mut sockets_b) = peer(remote);
        let tcp_a = sockets_a.add(tcp_socket());
        let tcp_b = sockets_b.add(tcp_socket());
        sockets_b
            .get_mut::<tcp::Socket>(tcp_b)
            .listen(5060)
            .unwrap();
        sockets_a
            .get_mut::<tcp::Socket>(tcp_a)
            .connect(ai.context(), (remote, 5060), 5060)
            .unwrap();
        let udp_a = sockets_a.add(udp_socket());
        let udp_b = sockets_b.add(udp_socket());
        sockets_a.get_mut::<udp::Socket>(udp_a).bind(5060).unwrap();
        sockets_b.get_mut::<udp::Socket>(udp_b).bind(5060).unwrap();
        const REQUEST: &[u8] = b"REGISTER sip:ims.example SIP/2.0\r\nContent-Length: 0\r\n\r\n";
        const RESPONSE: &[u8] = b"SIP/2.0 200 OK\r\nContent-Length: 0\r\n\r\n";
        sockets_a
            .get_mut::<udp::Socket>(udp_a)
            .send_slice(REQUEST, (remote, 5060))
            .unwrap();
        let (mut sent, mut tcp_request, mut udp_request, mut tcp_reply, mut udp_reply) =
            (false, false, false, false, false);
        for ms in (0..2000).step_by(10) {
            let now = Instant::from_millis(ms);
            ai.poll(now, &mut a, &mut sockets_a);
            move_packets(&mut a, &mut b);
            bi.poll(now, &mut b, &mut sockets_b);
            move_packets(&mut b, &mut a);
            let client = sockets_a.get_mut::<tcp::Socket>(tcp_a);
            if client.can_send() && !sent {
                assert_eq!(client.send_slice(REQUEST).unwrap(), REQUEST.len());
                sent = true;
            }
            if client.can_recv() {
                let mut buf = [0; 128];
                let n = client.recv_slice(&mut buf).unwrap();
                assert_eq!(&buf[..n], RESPONSE);
                tcp_reply = true;
            }
            let server = sockets_b.get_mut::<tcp::Socket>(tcp_b);
            if server.can_recv() {
                let mut buf = [0; 128];
                let n = server.recv_slice(&mut buf).unwrap();
                assert_eq!(&buf[..n], REQUEST);
                tcp_request = true;
                assert_eq!(server.send_slice(RESPONSE).unwrap(), RESPONSE.len());
            }
            let server = sockets_b.get_mut::<udp::Socket>(udp_b);
            if server.can_recv() {
                let (data, meta) = server.recv().unwrap();
                assert_eq!(data, REQUEST);
                udp_request = true;
                server.send_slice(RESPONSE, meta.endpoint).unwrap();
            }
            let client = sockets_a.get_mut::<udp::Socket>(udp_a);
            if client.can_recv() {
                let (data, _) = client.recv().unwrap();
                assert_eq!(data, RESPONSE);
                udp_reply = true;
            }
            if tcp_reply && udp_reply {
                break;
            }
        }
        assert!(sent && tcp_request && udp_request && tcp_reply && udp_reply);
    }

    #[test]
    fn queues_are_bounded_and_close_is_terminal() {
        let mut device = TunnelIpDevice::default();
        for _ in 0..QUEUE_PACKETS {
            device.transmit(Instant::ZERO).unwrap().consume(20, |_| ());
            device.push_inbound(Zeroizing::new(vec![0; 20])).unwrap();
        }
        assert!(device.transmit(Instant::ZERO).is_none());
        assert!(device.receive(Instant::ZERO).is_none());
        assert_eq!(
            device.push_inbound(Zeroizing::new(vec![0; 20])),
            Err(DeviceError::QueueFull)
        );
        device.pop_outbound();
        assert!(device.receive(Instant::ZERO).is_some());
        device.close();
        assert!(device.pop_outbound().is_none());
        assert!(device.receive(Instant::ZERO).is_none());
        assert!(device.transmit(Instant::ZERO).is_none());
        assert_eq!(
            device.push_inbound(Zeroizing::new(vec![0; 20])),
            Err(DeviceError::Closed)
        );
    }
}
