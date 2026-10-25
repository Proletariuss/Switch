//! Session-owned ESP data plane. Does not publish IMS or tunnel readiness.
use crate::{
    ChildTrafficSelectors, DatagramTransportError, EpdgDatagramTransport, PendingChildSa,
    TrafficSelector, TunnelConfiguration,
};
use proletarius_ike::esp::{EspError, EspReceiver, EspSender};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use zeroize::Zeroizing;

/// IKE packets are deliberately still untrusted here: the IKE owner must
/// authenticate them before processing DPD, DELETE, or rekey.
pub enum TunnelEvent {
    Ip(Zeroizing<Vec<u8>>),
    UnauthenticatedIke(Vec<u8>),
}

#[derive(Debug, Default)]
pub struct TunnelRunStats {
    pub received_ip: u64,
    pub discarded_packets: u64,
    pub received_ike: u64,
}

/// Only packet counts cross the tunnel owner boundary. No identities, keys,
/// addresses, payloads or per-packet timing are retained for diagnostics.
#[derive(Clone, Default)]
pub struct TunnelTrafficProbe(Arc<TunnelTrafficCounters>);

#[derive(Default)]
struct TunnelTrafficCounters {
    sent_esp: AtomicU64,
    received_esp: AtomicU64,
    accepted_ip: AtomicU64,
    rejected_esp: AtomicU64,
    received_ike: AtomicU64,
    received_keepalive: AtomicU64,
    invalid_datagram: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TunnelTrafficSnapshot {
    pub sent_esp: u64,
    pub received_esp: u64,
    pub accepted_ip: u64,
    pub rejected_esp: u64,
    pub received_ike: u64,
    pub received_keepalive: u64,
    pub invalid_datagram: u64,
}

impl TunnelTrafficProbe {
    pub fn snapshot(&self) -> TunnelTrafficSnapshot {
        TunnelTrafficSnapshot {
            sent_esp: self.0.sent_esp.load(Ordering::Relaxed),
            received_esp: self.0.received_esp.load(Ordering::Relaxed),
            accepted_ip: self.0.accepted_ip.load(Ordering::Relaxed),
            rejected_esp: self.0.rejected_esp.load(Ordering::Relaxed),
            received_ike: self.0.received_ike.load(Ordering::Relaxed),
            received_keepalive: self.0.received_keepalive.load(Ordering::Relaxed),
            invalid_datagram: self.0.invalid_datagram.load(Ordering::Relaxed),
        }
    }
}

/// Shared with the IKE owner so control retirement cannot precede old-key grace.
pub(crate) const OLD_CHILD_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// The caller sends already encrypted IKE packets; plaintext control frames
/// never belong on this queue. Inner IP is encrypted by the data plane.
pub enum TunnelCommand {
    ReplaceChild {
        plane: Box<ChildDataPlane>,
        installed: tokio::sync::oneshot::Sender<()>,
    },
    RetireOldChild {
        retired: tokio::sync::oneshot::Sender<()>,
    },
    ConfirmedIke {
        packet: Vec<u8>,
        sent: tokio::sync::oneshot::Sender<()>,
    },
    Ip(Zeroizing<Vec<u8>>),
    EncryptedIke(Vec<u8>),
    /// Send the authenticated IKE DELETE acknowledgement before releasing ESP.
    CloseAfterIke(Vec<u8>),
}

#[derive(Debug)]
pub enum ChildDataError {
    PeerDeleted,
    InvalidControlPacket,
    EventConsumerUnavailable,
    SendTimeout,
    UnsupportedAlgorithm,
    MissingConfiguration,
    PolicyRejected(PolicyRejectReason),
    Esp(EspError),
    Transport(DatagramTransportError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyRejectReason {
    MalformedPacket,
    UnsupportedIpv6Extension,
    UnassignedLocalAddress,
    SourceSelector,
    DestinationSelector,
}
impl std::fmt::Display for ChildDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PeerDeleted => f.write_str("peer deleted the IKE SA; tunnel closed"),
            Self::InvalidControlPacket => {
                f.write_str("tunnel IKE command must contain an encrypted SK payload")
            }
            Self::EventConsumerUnavailable => {
                f.write_str("tunnel event consumer closed or backlogged; session must be rebuilt")
            }
            Self::SendTimeout => f.write_str("tunnel datagram send timed out"),
            Self::UnsupportedAlgorithm => {
                f.write_str("negotiated ESP algorithm is not implemented by the native data plane")
            }
            Self::MissingConfiguration => {
                f.write_str("ESP data plane requires an assigned internal address")
            }
            Self::PolicyRejected(_) => {
                f.write_str("inner packet is malformed or outside negotiated traffic selectors")
            }
            Self::Esp(e) => e.fmt(f),
            Self::Transport(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for ChildDataError {}

pub struct ChildDataPlane {
    sender: EspSender,
    receiver: EspReceiver,
    old_receiver: Option<(EspReceiver, tokio::time::Instant)>,
    selectors: ChildTrafficSelectors,
    configuration: TunnelConfiguration,
    traffic: TunnelTrafficProbe,
}

impl PendingChildSa {
    /// Consumes the negotiated keys, so this value cannot accidentally install
    /// a second sender with its nonce counter reset.
    pub fn into_data_plane(mut self) -> Result<ChildDataPlane, ChildDataError> {
        let suite = (
            self.selection.encryption_transform,
            self.selection.integrity_transform,
        );
        let (sender, receiver) = match suite {
            (20, None)
                if self.selection.integrity_key_len == 0
                    && self.keys.initiator_integrity.is_empty()
                    && self.keys.responder_integrity.is_empty() =>
            {
                (
                    EspSender::new(
                        self.selection.remote_spi,
                        Zeroizing::new(std::mem::take(&mut self.keys.initiator_encryption)),
                    )
                    .map_err(ChildDataError::Esp)?,
                    EspReceiver::new(
                        self.selection.local_spi,
                        Zeroizing::new(std::mem::take(&mut self.keys.responder_encryption)),
                    )
                    .map_err(ChildDataError::Esp)?,
                )
            }
            (12, Some(integrity @ (2 | 12 | 14))) => (
                EspSender::new_cbc(
                    self.selection.remote_spi,
                    Zeroizing::new(std::mem::take(&mut self.keys.initiator_encryption)),
                    integrity,
                    Zeroizing::new(std::mem::take(&mut self.keys.initiator_integrity)),
                )
                .map_err(ChildDataError::Esp)?,
                EspReceiver::new_cbc(
                    self.selection.local_spi,
                    Zeroizing::new(std::mem::take(&mut self.keys.responder_encryption)),
                    integrity,
                    Zeroizing::new(std::mem::take(&mut self.keys.responder_integrity)),
                )
                .map_err(ChildDataError::Esp)?,
            ),
            _ => return Err(ChildDataError::UnsupportedAlgorithm),
        };
        let configuration = self
            .configuration
            .ok_or(ChildDataError::MissingConfiguration)?;
        if configuration.ipv4_addresses.is_empty() && configuration.ipv6_addresses.is_empty() {
            return Err(ChildDataError::MissingConfiguration);
        }
        Ok(ChildDataPlane {
            sender,
            receiver,
            old_receiver: None,
            selectors: self.traffic_selectors,
            configuration,
            traffic: TunnelTrafficProbe::default(),
        })
    }

    /// Compatibility gate for callers that still promise a GCM-only proposal
    /// set. It deliberately refuses a negotiated CBC suite.
    pub fn into_gcm_data_plane(self) -> Result<ChildDataPlane, ChildDataError> {
        if self.selection.encryption_transform != 20 || self.selection.integrity_transform.is_some()
        {
            return Err(ChildDataError::UnsupportedAlgorithm);
        }
        self.into_data_plane()
    }
}

impl ChildDataPlane {
    pub fn traffic_probe(&self) -> TunnelTrafficProbe {
        self.traffic.clone()
    }

    /// Single socket receive owner. Cancellation drops keys and transport.
    /// NAT keepalive is not a liveness proof; IMS/DPD owners remain responsible
    /// for authenticated health checks. Queue saturation fails visibly rather
    /// than silently losing an IKE control packet or leaving a fake-online SA.
    pub async fn run(
        self,
        transport: EpdgDatagramTransport,
        commands: tokio::sync::mpsc::Receiver<TunnelCommand>,
        events: tokio::sync::mpsc::Sender<TunnelEvent>,
        shutdown: tokio::sync::oneshot::Receiver<Vec<u8>>,
        stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<TunnelRunStats, ChildDataError> {
        self.run_with_keepalive(
            transport,
            commands,
            events,
            shutdown,
            stop,
            tokio::time::Duration::from_secs(20),
        )
        .await
    }
    pub async fn run_with_keepalive(
        mut self,
        transport: EpdgDatagramTransport,
        mut commands: tokio::sync::mpsc::Receiver<TunnelCommand>,
        events: tokio::sync::mpsc::Sender<TunnelEvent>,
        mut shutdown: tokio::sync::oneshot::Receiver<Vec<u8>>,
        mut stop: tokio::sync::watch::Receiver<bool>,
        keepalive_interval: tokio::time::Duration,
    ) -> Result<TunnelRunStats, ChildDataError> {
        use tokio::time::{Duration, MissedTickBehavior, timeout};
        let mut stats = TunnelRunStats::default();
        let mut keepalive = tokio::time::interval(keepalive_interval.max(Duration::from_millis(1)));
        keepalive.set_missed_tick_behavior(MissedTickBehavior::Skip);
        keepalive.tick().await;
        loop {
            if *stop.borrow() {
                return Ok(stats);
            }
            tokio::select! {
                biased;
                packet=&mut shutdown=>{
                    let packet=packet.map_err(|_|ChildDataError::EventConsumerUnavailable)?;
                    send_priority_shutdown(&transport,&packet).await?;
                    return Ok(stats);
                },
                changed=stop.changed()=>{if changed.is_err() || *stop.borrow() {return Ok(stats);}},
                _=events.closed()=>return Err(ChildDataError::EventConsumerUnavailable),
                command=commands.recv()=>{
                    let Some(command)=command else {return Ok(stats);};
                    let command=match command {
                        TunnelCommand::ReplaceChild{plane,installed}=>{
                            let mut plane=*plane;
                            // The probe belongs to the IKE generation, not an
                            // individual CHILD SA. Keep its counters across rekey.
                            plane.traffic=self.traffic.clone();
                            let old=std::mem::replace(&mut self,plane);
                            self.old_receiver=Some((old.receiver,tokio::time::Instant::now()+OLD_CHILD_GRACE));
                            let _=installed.send(());
                            continue;
                        },
                        TunnelCommand::RetireOldChild{retired}=>{self.old_receiver=None;let _=retired.send(());continue;},
                        TunnelCommand::ConfirmedIke{packet,sent}=>{
                            if !proletarius_ike::IkeHeader::decode(&packet).is_ok_and(|(header,body)|header.next_payload==46 && body.len()>=29){return Err(ChildDataError::InvalidControlPacket);}
                            tokio::time::timeout(Duration::from_secs(5),transport.send_ike(&packet)).await.map_err(|_|ChildDataError::SendTimeout)?.map_err(ChildDataError::Transport)?;
                            let _=sent.send(());continue;
                        },
                        command=>command,
                    };
                    let close_after_send = matches!(&command, TunnelCommand::CloseAfterIke(_));
                    let peer_deleted = matches!(&command, TunnelCommand::CloseAfterIke(_));
                    let operation=timeout(Duration::from_secs(5),async {
                        match command {
                            TunnelCommand::ReplaceChild{..}|TunnelCommand::RetireOldChild{..}|TunnelCommand::ConfirmedIke{..}=>unreachable!("handled before physical I/O"),
                            TunnelCommand::Ip(packet)=>self.send_ip(&transport,&packet).await,
                            TunnelCommand::EncryptedIke(packet)
                            | TunnelCommand::CloseAfterIke(packet)=>{
                                if !proletarius_ike::IkeHeader::decode(&packet).is_ok_and(|(header,body)|header.next_payload==46 && body.len()>=29) {
                                    return Err(ChildDataError::InvalidControlPacket);
                                }
                                transport.send_ike(&packet).await.map_err(ChildDataError::Transport)
                            },
                        }
                    });
                    tokio::pin!(operation);
                    tokio::select! {
                        biased;
                        packet=&mut shutdown=>{
                            let packet=packet.map_err(|_|ChildDataError::EventConsumerUnavailable)?;
                            send_priority_shutdown(&transport,&packet).await?;
                            return Ok(stats);
                        },
                        result=&mut operation=>{
                            result.map_err(|_|ChildDataError::SendTimeout)??;
                        },
                    }
                    if close_after_send {
                        return if peer_deleted {
                            Err(ChildDataError::PeerDeleted)
                        } else {
                            Ok(stats)
                        };
                    }
                },
                _=keepalive.tick()=>{
                    let operation=timeout(Duration::from_secs(5),transport.send_keepalive());
                    tokio::pin!(operation);
                    tokio::select! {
                        biased;
                        packet=&mut shutdown=>{
                            let packet=packet.map_err(|_|ChildDataError::EventConsumerUnavailable)?;
                            send_priority_shutdown(&transport,&packet).await?;
                            return Ok(stats);
                        },
                        result=&mut operation=>{
                            result.map_err(|_|ChildDataError::SendTimeout)?
                                .map_err(ChildDataError::Transport)?;
                        },
                    }
                },
                packet=transport.receive_packet()=>{
                    let event=match packet {
                        Ok(crate::EpdgPacket::Esp(packet))=>{
                            self.traffic.0.received_esp.fetch_add(1,Ordering::Relaxed);
                            match self.open_esp(&packet) {
                                Ok(ip)=>{
                                    self.traffic.0.accepted_ip.fetch_add(1,Ordering::Relaxed);
                                    stats.received_ip=stats.received_ip.saturating_add(1);
                                    Some(TunnelEvent::Ip(ip))
                                },
                                Err(ChildDataError::Esp(_)|ChildDataError::PolicyRejected(_))=>{
                                    self.traffic.0.rejected_esp.fetch_add(1,Ordering::Relaxed);
                                    stats.discarded_packets=stats.discarded_packets.saturating_add(1);
                                    None
                                },
                                Err(error)=>return Err(error),
                            }
                        },
                        Ok(crate::EpdgPacket::Ike(packet))=>{
                            self.traffic.0.received_ike.fetch_add(1,Ordering::Relaxed);
                            stats.received_ike=stats.received_ike.saturating_add(1);
                            Some(TunnelEvent::UnauthenticatedIke(packet))
                        },
                        Ok(crate::EpdgPacket::Keepalive)=>{
                            self.traffic.0.received_keepalive.fetch_add(1,Ordering::Relaxed);
                            None
                        },
                        Err(DatagramTransportError::InvalidEsp|DatagramTransportError::InvalidIke(_))=>{
                            self.traffic.0.invalid_datagram.fetch_add(1,Ordering::Relaxed);
                            stats.discarded_packets=stats.discarded_packets.saturating_add(1);
                            None
                        },
                        Err(error)=>return Err(ChildDataError::Transport(error)),
                    };
                    if let Some(event)=event {
                        let delivery=events.send(event);
                        tokio::pin!(delivery);
                        tokio::select! {
                            biased;
                            packet=&mut shutdown=>{
                                let packet=packet.map_err(|_|ChildDataError::EventConsumerUnavailable)?;
                                send_priority_shutdown(&transport,&packet).await?;
                                return Ok(stats);
                            },
                            delivered=timeout(Duration::from_millis(250),&mut delivery)=>{
                                delivered.map_err(|_|ChildDataError::EventConsumerUnavailable)?
                                    .map_err(|_|ChildDataError::EventConsumerUnavailable)?;
                            },
                        }
                    }
                },
            }
        }
    }
    pub fn configuration(&self) -> &TunnelConfiguration {
        &self.configuration
    }
    pub fn seal_ip(&mut self, packet: &[u8]) -> Result<Vec<u8>, ChildDataError> {
        self.check_policy(packet, true)?;
        self.sender.seal(packet).map_err(ChildDataError::Esp)
    }
    pub fn open_esp(&mut self, packet: &[u8]) -> Result<Zeroizing<Vec<u8>>, ChildDataError> {
        if self
            .old_receiver
            .as_ref()
            .is_some_and(|(_, expires)| tokio::time::Instant::now() >= *expires)
        {
            self.old_receiver = None;
        }
        let plain = match self.receiver.open(packet) {
            Ok(plain) => plain,
            Err(error) => {
                if let Some((old, _)) = &mut self.old_receiver {
                    old.open(packet).map_err(ChildDataError::Esp)?
                } else {
                    return Err(ChildDataError::Esp(error));
                }
            }
        };
        self.check_policy(&plain, false)?;
        Ok(plain)
    }
    /// Uses the existing selected ePDG channel, including SOCKS/TUN routing.
    /// No DNS resolution, fallback socket, or host route changes occur here.
    pub async fn send_ip(
        &mut self,
        transport: &EpdgDatagramTransport,
        packet: &[u8],
    ) -> Result<(), ChildDataError> {
        let esp = self.seal_ip(packet)?;
        transport
            .send_esp(&esp)
            .await
            .map_err(ChildDataError::Transport)?;
        self.traffic.0.sent_esp.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn check_policy(&self, packet: &[u8], outbound: bool) -> Result<(), ChildDataError> {
        let malformed = || ChildDataError::PolicyRejected(PolicyRejectReason::MalformedPacket);
        let (source, destination, protocol, offset) = match packet.first().map(|b| b >> 4) {
            Some(4) if packet.len() >= 20 => {
                let offset = usize::from(packet[0] & 15) * 4;
                if offset < 20
                    || offset > packet.len()
                    || usize::from(u16::from_be_bytes([packet[2], packet[3]])) != packet.len()
                    || u16::from_be_bytes([packet[6], packet[7]]) & 0x3fff != 0
                {
                    return Err(malformed());
                }
                (
                    IpAddr::V4(Ipv4Addr::from(
                        <[u8; 4]>::try_from(&packet[12..16]).unwrap(),
                    )),
                    IpAddr::V4(Ipv4Addr::from(
                        <[u8; 4]>::try_from(&packet[16..20]).unwrap(),
                    )),
                    packet[9],
                    offset,
                )
            }
            Some(6) if packet.len() >= 40 => {
                if usize::from(u16::from_be_bytes([packet[4], packet[5]])) + 40 != packet.len() {
                    return Err(malformed());
                }
                // IPv6 ESP is a terminal upper-layer protocol for traffic-
                // selector purposes. IMS sec-agree deliberately produces an
                // inner ESP packet which is then carried by this CHILD_SA;
                // its ports are encrypted and therefore remain `None` below.
                // Other extension headers still require explicit bounded
                // walking or fragment reassembly before policy can be
                // applied safely.
                if matches!(packet[6], 0 | 43 | 44 | 51 | 60) {
                    return Err(ChildDataError::PolicyRejected(
                        PolicyRejectReason::UnsupportedIpv6Extension,
                    ));
                }
                (
                    IpAddr::V6(Ipv6Addr::from(
                        <[u8; 16]>::try_from(&packet[8..24]).unwrap(),
                    )),
                    IpAddr::V6(Ipv6Addr::from(
                        <[u8; 16]>::try_from(&packet[24..40]).unwrap(),
                    )),
                    packet[6],
                    40,
                )
            }
            _ => return Err(malformed()),
        };
        let (source_port, destination_port) = match protocol {
            6 | 17 => {
                let ports = packet.get(offset..offset + 4).ok_or_else(malformed)?;
                (
                    Some(u16::from_be_bytes([ports[0], ports[1]])),
                    Some(u16::from_be_bytes([ports[2], ports[3]])),
                )
            }
            1 | 58 => {
                let icmp = packet.get(offset..offset + 2).ok_or_else(malformed)?;
                let port = Some(u16::from_be_bytes([icmp[0], icmp[1]]));
                (port, port)
            }
            _ => (None, None),
        };
        let local = if outbound { source } else { destination };
        let assigned = match local {
            IpAddr::V4(ip) => self.configuration.ipv4_addresses.contains(&ip),
            IpAddr::V6(ip) => self
                .configuration
                .ipv6_addresses
                .iter()
                .any(|(address, _)| *address == ip),
        };
        let (sources, destinations) = if outbound {
            (&self.selectors.initiator, &self.selectors.responder)
        } else {
            (&self.selectors.responder, &self.selectors.initiator)
        };
        if !assigned {
            return Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::UnassignedLocalAddress,
            ));
        }
        if !matches_selector(sources, source, protocol, source_port) {
            return Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::SourceSelector,
            ));
        }
        if !matches_selector(destinations, destination, protocol, destination_port) {
            return Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::DestinationSelector,
            ));
        }
        Ok(())
    }
}

async fn send_priority_shutdown(
    transport: &EpdgDatagramTransport,
    packet: &[u8],
) -> Result<(), ChildDataError> {
    if !proletarius_ike::IkeHeader::decode(packet)
        .is_ok_and(|(header, body)| header.next_payload == 46 && body.len() >= 29)
    {
        return Err(ChildDataError::InvalidControlPacket);
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        transport.send_ike(packet),
    )
    .await
    .map_err(|_| ChildDataError::SendTimeout)?
    .map_err(ChildDataError::Transport)
}

fn matches_selector(
    selectors: &[TrafficSelector],
    address: IpAddr,
    protocol: u8,
    port: Option<u16>,
) -> bool {
    selectors.iter().any(|s| {
        s.start_address.is_ipv4() == address.is_ipv4()
            && s.start_address <= address
            && s.end_address >= address
            && (s.protocol == 0 || s.protocol == protocol)
            && port.map_or(s.start_port == 0 && s.end_port == 65535, |port| {
                s.start_port <= port && s.end_port >= port
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broad_selectors() -> ChildTrafficSelectors {
        let selector = || {
            vec![TrafficSelector {
                protocol: 0,
                start_port: 0,
                end_port: 65535,
                start_address: "0.0.0.0".parse().unwrap(),
                end_address: "255.255.255.255".parse().unwrap(),
            }]
        };
        ChildTrafficSelectors {
            initiator: selector(),
            responder: selector(),
        }
    }

    fn ipv4_packet(source: [u8; 4], destination: [u8; 4]) -> Vec<u8> {
        let mut packet = vec![0; 20];
        packet[0] = 0x45;
        packet[3] = 20;
        packet[12..16].copy_from_slice(&source);
        packet[16..20].copy_from_slice(&destination);
        packet
    }

    fn runtime_plane() -> ChildDataPlane {
        ChildDataPlane {
            sender: EspSender::new(1, Zeroizing::new(vec![1; 20])).unwrap(),
            receiver: EspReceiver::new(2, Zeroizing::new(vec![2; 20])).unwrap(),
            old_receiver: None,
            selectors: broad_selectors(),
            configuration: TunnelConfiguration {
                ipv4_addresses: vec![Ipv4Addr::new(10, 0, 0, 2)],
                ..Default::default()
            },
            traffic: TunnelTrafficProbe::default(),
        }
    }

    #[test]
    fn policy_rejection_reasons_are_bounded_and_directional() {
        let plane = runtime_plane();
        let packet = ipv4_packet([10, 0, 0, 3], [10, 0, 0, 9]);
        assert!(matches!(
            plane.check_policy(&packet, true),
            Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::UnassignedLocalAddress
            ))
        ));

        let mut plane = runtime_plane();
        plane.selectors.initiator[0].start_address = "10.0.0.3".parse().unwrap();
        plane.selectors.initiator[0].end_address = "10.0.0.3".parse().unwrap();
        let packet = ipv4_packet([10, 0, 0, 2], [10, 0, 0, 9]);
        assert!(matches!(
            plane.check_policy(&packet, true),
            Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::SourceSelector
            ))
        ));

        let mut plane = runtime_plane();
        plane.selectors.responder[0].start_address = "10.0.0.8".parse().unwrap();
        plane.selectors.responder[0].end_address = "10.0.0.8".parse().unwrap();
        assert!(matches!(
            plane.check_policy(&packet, true),
            Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::DestinationSelector
            ))
        ));

        assert!(matches!(
            plane.check_policy(&packet[..19], true),
            Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::MalformedPacket
            ))
        ));
    }

    #[test]
    fn ipv6_ims_esp_is_a_terminal_protocol_under_broad_child_selectors() {
        use proletarius_ims::{
            aka_exchange::SecurityKeys,
            security_policy::{LocalEndpoints, SecurityPolicy},
            security_transport::SecurityTransport,
        };

        let local: Ipv6Addr = "2001:db8::2".parse().unwrap();
        let remote: Ipv6Addr = "2001:db8::9".parse().unwrap();
        let selector = TrafficSelector {
            protocol: 0,
            start_port: 0,
            end_port: u16::MAX,
            start_address: Ipv6Addr::UNSPECIFIED.into(),
            end_address: Ipv6Addr::from(u128::MAX).into(),
        };
        let plane = ChildDataPlane {
            sender: EspSender::new(1, Zeroizing::new(vec![1; 20])).unwrap(),
            receiver: EspReceiver::new(2, Zeroizing::new(vec![2; 20])).unwrap(),
            old_receiver: None,
            selectors: ChildTrafficSelectors {
                initiator: vec![selector.clone()],
                responder: vec![selector],
            },
            configuration: TunnelConfiguration {
                ipv6_addresses: vec![(local, 128)],
                ..Default::default()
            },
            traffic: TunnelTrafficProbe::default(),
        };
        let mut packet = vec![0_u8; 48];
        packet[0] = 0x60;
        packet[4..6].copy_from_slice(&8_u16.to_be_bytes());
        packet[6] = 50;
        packet[7] = 64;
        packet[8..24].copy_from_slice(&local.octets());
        packet[24..40].copy_from_slice(&remote.octets());
        packet[40..44].copy_from_slice(&0x1020_3040_u32.to_be_bytes());
        packet[44..48].copy_from_slice(&1_u32.to_be_bytes());

        assert!(plane.check_policy(&packet, true).is_ok());

        let offer = proletarius_ims::security_offer::parse_security_server(
            "ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc;spi-c=111;spi-s=222;port-c=6050;port-s=6060",
        )
        .unwrap()
        .remove(0);
        let policy = SecurityPolicy::negotiated(
            local.into(),
            remote.into(),
            LocalEndpoints {
                spi_c: 333,
                spi_s: 444,
                port_c: 5050,
                port_s: 5060,
            },
            &offer,
            SecurityKeys {
                ck: Zeroizing::new([0x11; 16]),
                ik: Zeroizing::new([0x22; 16]),
            },
        )
        .unwrap();
        let mut ims = SecurityTransport::new(policy).unwrap();
        let mut plain = vec![0_u8; 60];
        plain[0] = 0x60;
        plain[4..6].copy_from_slice(&20_u16.to_be_bytes());
        plain[6] = 6;
        plain[7] = 64;
        plain[8..24].copy_from_slice(&local.octets());
        plain[24..40].copy_from_slice(&remote.octets());
        plain[40..42].copy_from_slice(&5050_u16.to_be_bytes());
        plain[42..44].copy_from_slice(&6060_u16.to_be_bytes());
        plain[52] = 0x50;
        let protected = ims.outbound(&plain).unwrap();
        assert_eq!(protected[6], 50);
        assert!(plane.check_policy(&protected, true).is_ok());

        let mut protocol_narrowed = plane;
        protocol_narrowed.selectors.initiator[0].protocol = 6;
        assert!(matches!(
            protocol_narrowed.check_policy(&protected, true),
            Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::SourceSelector
            ))
        ));
        protocol_narrowed.selectors.initiator[0].protocol = 50;
        protocol_narrowed.selectors.initiator[0].start_port = 5060;
        protocol_narrowed.selectors.initiator[0].end_port = 5060;
        assert!(matches!(
            protocol_narrowed.check_policy(&protected, true),
            Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::SourceSelector
            ))
        ));
    }

    #[test]
    fn ipv6_nested_child_and_ims_esp_preserve_inbound_policy_and_replay_checks() {
        use proletarius_ims::{
            aka_exchange::SecurityKeys,
            security_offer::parse_security_server,
            security_policy::{LocalEndpoints, SecurityPolicy},
            security_transport::{InboundDropReason, SecurityTransport, TransportDisposition},
        };

        fn ims(
            local: Ipv6Addr,
            remote: Ipv6Addr,
            own: LocalEndpoints,
            peer: LocalEndpoints,
        ) -> SecurityTransport {
            let offer = parse_security_server(&format!(
                "ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc;spi-c={};spi-s={};port-c={};port-s={}",
                peer.spi_c, peer.spi_s, peer.port_c, peer.port_s
            ))
            .unwrap()
            .remove(0);
            SecurityTransport::new(
                SecurityPolicy::negotiated(
                    local.into(),
                    remote.into(),
                    own,
                    &offer,
                    SecurityKeys {
                        ck: Zeroizing::new([0x11; 16]),
                        ik: Zeroizing::new([0x22; 16]),
                    },
                )
                .unwrap(),
            )
            .unwrap()
        }

        fn child(local: Ipv6Addr, tx: u8, rx: u8) -> ChildDataPlane {
            let selector = TrafficSelector {
                protocol: 0,
                start_port: 0,
                end_port: u16::MAX,
                start_address: Ipv6Addr::UNSPECIFIED.into(),
                end_address: Ipv6Addr::from(u128::MAX).into(),
            };
            ChildDataPlane {
                sender: EspSender::new(tx.into(), Zeroizing::new(vec![tx; 20])).unwrap(),
                receiver: EspReceiver::new(rx.into(), Zeroizing::new(vec![rx; 20])).unwrap(),
                old_receiver: None,
                selectors: ChildTrafficSelectors {
                    initiator: vec![selector.clone()],
                    responder: vec![selector],
                },
                configuration: TunnelConfiguration {
                    ipv6_addresses: vec![(local, 128)],
                    ..Default::default()
                },
                traffic: TunnelTrafficProbe::default(),
            }
        }

        fn tcp(source: Ipv6Addr, destination: Ipv6Addr, ports: (u16, u16)) -> Vec<u8> {
            let payload = b"IMS nested ESP";
            let tcp_len = 20 + payload.len();
            let mut packet = vec![0_u8; 40 + tcp_len];
            packet[0] = 0x60;
            packet[4..6].copy_from_slice(&(tcp_len as u16).to_be_bytes());
            packet[6] = 6;
            packet[7] = 64;
            packet[8..24].copy_from_slice(&source.octets());
            packet[24..40].copy_from_slice(&destination.octets());
            packet[40..42].copy_from_slice(&ports.0.to_be_bytes());
            packet[42..44].copy_from_slice(&ports.1.to_be_bytes());
            packet[52] = 0x50;
            packet[53] = 0x18;
            packet[54..56].copy_from_slice(&4096_u16.to_be_bytes());
            packet[60..].copy_from_slice(payload);
            let mut sum = tcp_len as u32 + 6;
            for bytes in [&packet[8..40], &packet[40..]] {
                for pair in bytes.chunks(2) {
                    sum += u32::from(u16::from_be_bytes([
                        pair[0],
                        pair.get(1).copied().unwrap_or(0),
                    ]));
                }
            }
            while sum >> 16 != 0 {
                sum = (sum & 0xffff) + (sum >> 16);
            }
            packet[56..58].copy_from_slice(&(!(sum as u16)).to_be_bytes());
            packet
        }

        // Every ingress call seals a fresh outer sequence. Reusing an inner
        // packet below must exercise the IMS receiver, not outer anti-replay.
        fn ingress(
            sender: &mut ChildDataPlane,
            receiver: &mut ChildDataPlane,
            inner: &[u8],
        ) -> Zeroizing<Vec<u8>> {
            let outer = sender.seal_ip(inner).unwrap();
            let opened = receiver.open_esp(&outer).unwrap();
            assert_eq!(&opened[..], inner);
            opened
        }

        let local: Ipv6Addr = "2001:db8::2".parse().unwrap();
        let remote: Ipv6Addr = "2001:db8::9".parse().unwrap();
        let local_endpoints = LocalEndpoints {
            spi_c: 333,
            spi_s: 444,
            port_c: 5050,
            port_s: 5060,
        };
        let remote_endpoints = LocalEndpoints {
            spi_c: 111,
            spi_s: 222,
            port_c: 6050,
            port_s: 6060,
        };
        let mut local_ims = ims(local, remote, local_endpoints, remote_endpoints);
        let mut remote_ims = ims(remote, local, remote_endpoints, local_endpoints);
        let mut local_child = child(local, 1, 2);
        let mut remote_child = child(remote, 2, 1);

        let request = tcp(local, remote, (5050, 6060));
        let protected = local_ims.outbound(&request).unwrap();
        let opened = ingress(&mut local_child, &mut remote_child, &protected);
        assert_eq!(&remote_ims.inbound(&opened).unwrap()[..], &request[..]);

        let reply = tcp(remote, local, (6050, 5060));
        let protected = remote_ims.outbound(&reply).unwrap();
        assert_eq!(protected[6], 50);

        let mut tampered = protected.clone();
        *tampered.last_mut().unwrap() ^= 1;
        let opened = ingress(&mut remote_child, &mut local_child, &tampered);
        assert!(matches!(
            local_ims.route_inbound(opened).unwrap(),
            TransportDisposition::Drop(InboundDropReason::Authentication)
        ));

        let mut wrong_spi = protected.clone();
        wrong_spi[40..44].copy_from_slice(&999_u32.to_be_bytes());
        let opened = ingress(&mut remote_child, &mut local_child, &wrong_spi);
        assert!(matches!(
            local_ims.route_inbound(opened).unwrap(),
            TransportDisposition::Drop(InboundDropReason::FlowMismatch)
        ));

        // Bad inner authentication/SPI did not consume the valid inner sequence.
        let opened = ingress(&mut remote_child, &mut local_child, &protected);
        assert_eq!(&local_ims.inbound(&opened).unwrap()[..], &reply[..]);
        let opened = ingress(&mut remote_child, &mut local_child, &protected);
        assert!(matches!(
            local_ims.route_inbound(opened).unwrap(),
            TransportDisposition::Drop(InboundDropReason::Replay)
        ));

        // The CHILD selector cannot inspect encrypted TCP ports. Narrow-port
        // and TCP-only selectors must still reject inner protocol 50 ingress.
        local_child.selectors.responder[0].protocol = 6;
        let outer = remote_child.seal_ip(&protected).unwrap();
        assert!(matches!(
            local_child.open_esp(&outer),
            Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::SourceSelector
            ))
        ));
        local_child.selectors.responder[0].protocol = 50;
        local_child.selectors.responder[0].start_port = 6050;
        local_child.selectors.responder[0].end_port = 6050;
        let outer = remote_child.seal_ip(&protected).unwrap();
        assert!(matches!(
            local_child.open_esp(&outer),
            Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::SourceSelector
            ))
        ));
        local_child.selectors.responder[0].start_port = 0;
        local_child.selectors.responder[0].end_port = u16::MAX;

        // Assigned-address rejection occurs after valid outer authentication.
        let mut wrong_destination = protected.clone();
        wrong_destination[24..40]
            .copy_from_slice(&"2001:db8::3".parse::<Ipv6Addr>().unwrap().octets());
        let outer = remote_child.seal_ip(&wrong_destination).unwrap();
        assert!(matches!(
            local_child.open_esp(&outer),
            Err(ChildDataError::PolicyRejected(
                PolicyRejectReason::UnassignedLocalAddress
            ))
        ));

        // Broad CHILD address selectors still cannot bypass the IMS peer IP.
        let mut wrong_source = protected.clone();
        wrong_source[8..24].copy_from_slice(&"2001:db8::8".parse::<Ipv6Addr>().unwrap().octets());
        let outer = remote_child.sender.seal(&wrong_source).unwrap();
        let opened = local_child.open_esp(&outer).unwrap();
        assert!(matches!(
            local_ims.route_inbound(opened).unwrap(),
            TransportDisposition::Drop(InboundDropReason::FlowMismatch)
        ));

        // An authenticated inner SA with the expected SPI/key but wrong TCP
        // source port is rejected after decryption, independent of replay.
        let mut wrong_port_peer = ims(
            remote,
            local,
            LocalEndpoints {
                port_c: 6051,
                ..remote_endpoints
            },
            local_endpoints,
        );
        let wrong_port_tcp = tcp(remote, local, (6051, 5060));
        // Advance this independent peer past the sequence already accepted.
        wrong_port_peer.outbound(&wrong_port_tcp).unwrap();
        let wrong_port = wrong_port_peer.outbound(&wrong_port_tcp).unwrap();
        let opened = ingress(&mut remote_child, &mut local_child, &wrong_port);
        assert!(matches!(
            local_ims.route_inbound(opened).unwrap(),
            TransportDisposition::Drop(InboundDropReason::FlowMismatch)
        ));
    }

    #[tokio::test]
    async fn traffic_probe_survives_child_sa_replacement() {
        use tokio::{
            net::UdpSocket,
            sync::{mpsc, oneshot, watch},
            time::{Duration, timeout},
        };
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = peer.local_addr().unwrap();
        let transport = EpdgDatagramTransport::open(
            crate::ProxyMode::Direct(address.ip()),
            "unused.invalid",
            address.port(),
            None,
        )
        .await
        .unwrap()
        .enable_pinned_natt();
        let plane = runtime_plane();
        let probe = plane.traffic_probe();
        let (commands, receiver) = mpsc::channel(2);
        let (events, _incoming) = mpsc::channel(2);
        let (_shutdown, shutdown_rx) = oneshot::channel();
        let (stop, stop_rx) = watch::channel(false);
        let worker = tokio::spawn(plane.run(transport, receiver, events, shutdown_rx, stop_rx));
        let (installed, confirmed) = oneshot::channel();
        commands
            .send(TunnelCommand::ReplaceChild {
                plane: Box::new(runtime_plane()),
                installed,
            })
            .await
            .unwrap();
        confirmed.await.unwrap();
        commands
            .send(TunnelCommand::Ip(Zeroizing::new(ipv4_packet(
                [10, 0, 0, 2],
                [10, 0, 0, 9],
            ))))
            .await
            .unwrap();
        let mut packet = [0; 128];
        timeout(Duration::from_secs(1), peer.recv(&mut packet))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(probe.snapshot().sent_esp, 1);
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
    }

    fn protected_control(message_id: u32, frames: &[proletarius_ike::PayloadFrame]) -> Vec<u8> {
        proletarius_ike::sk::seal_ike_sk_gcm(
            proletarius_ike::IkeHeader {
                initiator_spi: 1,
                responder_spi: 2,
                next_payload: 0,
                version: 0x20,
                exchange: proletarius_ike::ExchangeType::Informational,
                flags: proletarius_ike::IkeHeader::INITIATOR_FLAG,
                message_id,
            },
            20,
            &[3; 20],
            frames,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn priority_shutdown_preempts_dpd_control_and_event_backlog() {
        use tokio::{
            net::UdpSocket,
            sync::{mpsc, oneshot, watch},
            time::{Duration, timeout},
        };
        timeout(Duration::from_secs(2), async {
            let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let address = peer.local_addr().unwrap();
            let transport = EpdgDatagramTransport::open(
                crate::ProxyMode::Direct(address.ip()),
                "unused.invalid",
                address.port(),
                None,
            )
            .await
            .unwrap();
            let (commands, rx) = mpsc::channel(2);
            commands
                .send(TunnelCommand::EncryptedIke(protected_control(3, &[])))
                .await
                .unwrap();
            commands
                .send(TunnelCommand::EncryptedIke(protected_control(4, &[])))
                .await
                .unwrap();
            let (events, _incoming) = mpsc::channel(1);
            assert!(
                events
                    .try_send(TunnelEvent::UnauthenticatedIke(vec![0]))
                    .is_ok()
            );
            let (shutdown, shutdown_rx) = oneshot::channel();
            let (_stop, stop_rx) = watch::channel(false);
            let delete = protected_control(
                5,
                &[proletarius_ike::PayloadFrame {
                    kind: 42,
                    critical: false,
                    body: vec![1, 0, 0, 0],
                }],
            );
            shutdown.send(delete.clone()).unwrap();
            let runtime =
                tokio::spawn(runtime_plane().run(transport, rx, events, shutdown_rx, stop_rx));
            let mut wire = [0; 512];
            let (size, _) = peer.recv_from(&mut wire).await.unwrap();
            assert_eq!(&wire[..size], delete.as_slice());
            assert_eq!(runtime.await.unwrap().unwrap().received_ike, 0);
            assert!(
                timeout(Duration::from_millis(50), peer.recv_from(&mut wire))
                    .await
                    .is_err(),
                "queued DPD must not be emitted after priority shutdown"
            );
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn priority_shutdown_escapes_event_backpressure() {
        use tokio::{
            net::UdpSocket,
            sync::{mpsc, oneshot, watch},
            time::{Duration, timeout},
        };
        timeout(Duration::from_secs(2), async {
            let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let address = peer.local_addr().unwrap();
            let transport = EpdgDatagramTransport::open(
                crate::ProxyMode::Direct(address.ip()),
                "unused.invalid",
                address.port(),
                None,
            )
            .await
            .unwrap();
            let (commands, rx) = mpsc::channel(1);
            let (events, _incoming) = mpsc::channel(1);
            assert!(
                events
                    .try_send(TunnelEvent::UnauthenticatedIke(vec![0]))
                    .is_ok()
            );
            let (shutdown, shutdown_rx) = oneshot::channel();
            let (_stop, stop_rx) = watch::channel(false);
            let runtime =
                tokio::spawn(runtime_plane().run(transport, rx, events, shutdown_rx, stop_rx));
            let dpd = protected_control(3, &[]);
            commands
                .send(TunnelCommand::EncryptedIke(dpd.clone()))
                .await
                .unwrap();
            let mut wire = [0; 512];
            let (size, source) = peer.recv_from(&mut wire).await.unwrap();
            assert_eq!(&wire[..size], dpd.as_slice());
            peer.send_to(&dpd, source).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let delete = protected_control(
                4,
                &[proletarius_ike::PayloadFrame {
                    kind: 42,
                    critical: false,
                    body: vec![1, 0, 0, 0],
                }],
            );
            shutdown.send(delete.clone()).unwrap();
            let (size, _) = peer.recv_from(&mut wire).await.unwrap();
            assert_eq!(&wire[..size], delete.as_slice());
            assert_eq!(runtime.await.unwrap().unwrap().received_ike, 1);
        })
        .await
        .unwrap();
    }

    #[test]
    fn cbc_child_sa_installs_128_and_256_bit_directional_keys_and_preserves_policy() {
        for encryption_length in [16, 32] {
            let initiator_encryption = vec![0x11; encryption_length];
            let initiator_integrity = vec![0x22; 32];
            let responder_encryption = vec![0x33; encryption_length];
            let responder_integrity = vec![0x44; 32];
            let child = PendingChildSa {
                configuration: Some(TunnelConfiguration {
                    ipv4_addresses: vec![Ipv4Addr::new(10, 0, 0, 2)],
                    ..Default::default()
                }),
                traffic_selectors: broad_selectors(),
                response: crate::IkeAuthResponse {
                    header: proletarius_ike::IkeHeader {
                        initiator_spi: 1,
                        responder_spi: 2,
                        next_payload: 46,
                        version: 0x20,
                        exchange: proletarius_ike::ExchangeType::IkeAuth,
                        flags: proletarius_ike::IkeHeader::RESPONSE_FLAG,
                        message_id: 1,
                    },
                    frames: vec![],
                },
                selection: crate::InitialChildSaSelection {
                    local_spi: 1,
                    remote_spi: 2,
                    encryption_transform: 12,
                    integrity_transform: Some(12),
                    encryption_key_len: encryption_length,
                    integrity_key_len: 32,
                },
                keys: proletarius_ike::ChildSaKeyMaterial {
                    initiator_encryption: initiator_encryption.clone(),
                    initiator_integrity: initiator_integrity.clone(),
                    responder_encryption: responder_encryption.clone(),
                    responder_integrity: responder_integrity.clone(),
                },
            };
            let mut plane = child.into_data_plane().unwrap();
            let outbound = ipv4_packet([10, 0, 0, 2], [10, 0, 0, 9]);
            let encrypted = plane.seal_ip(&outbound).unwrap();
            let mut network_receiver = EspReceiver::new_cbc(
                2,
                Zeroizing::new(initiator_encryption),
                12,
                Zeroizing::new(initiator_integrity),
            )
            .unwrap();
            assert_eq!(&*network_receiver.open(&encrypted).unwrap(), &outbound);

            let inbound = ipv4_packet([10, 0, 0, 9], [10, 0, 0, 2]);
            let mut network_sender = EspSender::new_cbc(
                1,
                Zeroizing::new(responder_encryption),
                12,
                Zeroizing::new(responder_integrity),
            )
            .unwrap();
            assert_eq!(
                &*plane
                    .open_esp(&network_sender.seal(&inbound).unwrap())
                    .unwrap(),
                &inbound
            );
        }
    }

    #[tokio::test]
    async fn socks_4500_cbc_sha256_round_trip_uses_the_negotiated_spi_and_keys() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::{TcpListener, UdpSocket},
            sync::oneshot,
            time::{Duration, timeout},
        };
        timeout(Duration::from_secs(3), async {
            let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy = tcp.local_addr().unwrap();
            let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let relay = udp.local_addr().unwrap();
            let outbound = ipv4_packet([10, 0, 0, 2], [10, 0, 0, 9]);
            let inbound = ipv4_packet([10, 0, 0, 9], [10, 0, 0, 2]);
            let expected_outbound = outbound.clone();
            let expected_inbound = inbound.clone();
            let (done, release) = oneshot::channel::<()>();
            let server = tokio::spawn(async move {
                let (mut control, _) = tcp.accept().await.unwrap();
                let mut greeting = [0; 3];
                control.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [5, 1, 0]);
                control.write_all(&[5, 0]).await.unwrap();
                let mut associate = [0; 10];
                control.read_exact(&mut associate).await.unwrap();
                assert_eq!(associate, [5, 3, 0, 1, 0, 0, 0, 0, 0, 0]);
                let mut reply = vec![5, 0, 0, 1, 127, 0, 0, 1];
                reply.extend_from_slice(&relay.port().to_be_bytes());
                control.write_all(&reply).await.unwrap();

                let mut wire = [0; 2048];
                let (size, source) = udp.recv_from(&mut wire).await.unwrap();
                let datagram = crate::decode_udp_datagram(&wire[..size]).unwrap();
                assert_eq!(
                    datagram.target,
                    crate::SocksTarget::Domain("epdg.invalid".into())
                );
                assert_eq!(datagram.port, 4500);
                assert_eq!(&datagram.payload[..4], &2_u32.to_be_bytes());
                let mut peer_receiver = EspReceiver::new_cbc(
                    2,
                    Zeroizing::new(vec![0x11; 32]),
                    12,
                    Zeroizing::new(vec![0x22; 32]),
                )
                .unwrap();
                assert_eq!(
                    &*peer_receiver.open(&datagram.payload).unwrap(),
                    &expected_outbound
                );

                let mut peer_sender = EspSender::new_cbc(
                    1,
                    Zeroizing::new(vec![0x33; 32]),
                    12,
                    Zeroizing::new(vec![0x44; 32]),
                )
                .unwrap();
                let esp = peer_sender.seal(&expected_inbound).unwrap();
                let reply = crate::encode_udp_domain("epdg.invalid", 4500, &esp).unwrap();
                udp.send_to(&reply, source).await.unwrap();
                release.await.unwrap();
            });
            let transport = EpdgDatagramTransport::open(
                crate::ProxyMode::Socks5UdpDomain,
                "epdg.invalid",
                4500,
                Some(proxy),
            )
            .await
            .unwrap();
            let mut plane = ChildDataPlane {
                sender: EspSender::new_cbc(
                    2,
                    Zeroizing::new(vec![0x11; 32]),
                    12,
                    Zeroizing::new(vec![0x22; 32]),
                )
                .unwrap(),
                receiver: EspReceiver::new_cbc(
                    1,
                    Zeroizing::new(vec![0x33; 32]),
                    12,
                    Zeroizing::new(vec![0x44; 32]),
                )
                .unwrap(),
                old_receiver: None,
                selectors: broad_selectors(),
                configuration: TunnelConfiguration {
                    ipv4_addresses: vec![Ipv4Addr::new(10, 0, 0, 2)],
                    ..Default::default()
                },
                traffic: TunnelTrafficProbe::default(),
            };
            plane.send_ip(&transport, &outbound).await.unwrap();
            let packet = transport.receive_packet().await.unwrap();
            let crate::EpdgPacket::Esp(packet) = packet else {
                panic!("CBC reply must be classified as ESP");
            };
            assert_eq!(&*plane.open_esp(&packet).unwrap(), &inbound);
            assert_eq!(plane.traffic_probe().snapshot().sent_esp, 1);
            done.send(()).unwrap();
            server.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn socks_runtime_carries_mixed_traffic_and_detects_control_loss() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::{TcpListener, UdpSocket},
            sync::{mpsc, oneshot, watch},
            time::{Duration, timeout},
        };
        for close_control in [false, true] {
            timeout(Duration::from_secs(3), async {
                let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let proxy = tcp.local_addr().unwrap();
                let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                let relay = udp.local_addr().unwrap();
                let (release, done) = oneshot::channel::<()>();
                let mut outgoing = vec![0; 28];
                outgoing[0] = 0x45;
                outgoing[3] = 28;
                outgoing[9] = 17;
                outgoing[12..16].copy_from_slice(&[10, 0, 0, 2]);
                outgoing[16..20].copy_from_slice(&[10, 0, 0, 9]);
                outgoing[20..24].copy_from_slice(&[0x13, 0xc4, 0x13, 0xc4]);
                outgoing[25] = 8;
                let expected = outgoing.clone();
                let server = tokio::spawn(async move {
                    let (mut control, _) = tcp.accept().await.unwrap();
                    let mut greeting = [0; 3];
                    control.read_exact(&mut greeting).await.unwrap();
                    assert_eq!(greeting, [5, 1, 0]);
                    control.write_all(&[5, 0]).await.unwrap();
                    let mut associate = [0; 10];
                    control.read_exact(&mut associate).await.unwrap();
                    assert_eq!(&associate[..3], &[5, 3, 0]);
                    let mut reply = vec![5, 0, 0, 1, 127, 0, 0, 1];
                    reply.extend_from_slice(&relay.port().to_be_bytes());
                    control.write_all(&reply).await.unwrap();
                    let mut buffer = [0; 2048];
                    let (size, source) = udp.recv_from(&mut buffer).await.unwrap();
                    let packet = crate::decode_udp_datagram(&buffer[..size]).unwrap();
                    assert_eq!(
                        packet.target,
                        crate::SocksTarget::Domain("epdg.invalid".into())
                    );
                    assert_eq!(packet.port, 4500);
                    let mut receiver = EspReceiver::new(1, Zeroizing::new(vec![1; 20])).unwrap();
                    assert_eq!(&*receiver.open(&packet.payload).unwrap(), &expected);
                    let mut incoming = expected;
                    incoming[15] = 9;
                    incoming[19] = 2;
                    let mut sender = EspSender::new(2, Zeroizing::new(vec![2; 20])).unwrap();
                    let esp = sender.seal(&incoming).unwrap();
                    let mut bad = esp.clone();
                    *bad.last_mut().unwrap() ^= 1;
                    let header = proletarius_ike::IkeHeader {
                        initiator_spi: 1,
                        responder_spi: 2,
                        next_payload: 0,
                        version: 0x20,
                        exchange: proletarius_ike::ExchangeType::Informational,
                        flags: 0,
                        message_id: 0,
                    };
                    let ike =
                        proletarius_ike::sk::seal_ike_sk_gcm(header, 20, &[3; 20], &[]).unwrap();
                    for data in [
                        vec![0xff],
                        vec![0x01],
                        bad,
                        esp.clone(),
                        esp,
                        proletarius_ike::encode_natt(&ike),
                    ] {
                        udp.send_to(
                            &crate::encode_udp_domain("epdg.invalid", 4500, &data).unwrap(),
                            source,
                        )
                        .await
                        .unwrap();
                    }
                    done.await.unwrap();
                });
                let transport = EpdgDatagramTransport::open(
                    crate::ProxyMode::Socks5UdpDomain,
                    "epdg.invalid",
                    4500,
                    Some(proxy),
                )
                .await
                .unwrap();
                let selector = || {
                    vec![TrafficSelector {
                        protocol: 0,
                        start_port: 0,
                        end_port: 65535,
                        start_address: "0.0.0.0".parse().unwrap(),
                        end_address: "255.255.255.255".parse().unwrap(),
                    }]
                };
                let plane = ChildDataPlane {
                    old_receiver: None,
                    sender: EspSender::new(1, Zeroizing::new(vec![1; 20])).unwrap(),
                    receiver: EspReceiver::new(2, Zeroizing::new(vec![2; 20])).unwrap(),
                    selectors: ChildTrafficSelectors {
                        initiator: selector(),
                        responder: selector(),
                    },
                    configuration: TunnelConfiguration {
                        ipv4_addresses: vec![Ipv4Addr::new(10, 0, 0, 2)],
                        ..Default::default()
                    },
                    traffic: TunnelTrafficProbe::default(),
                };
                let probe = plane.traffic_probe();
                let (commands, rx) = mpsc::channel(2);
                let (events, mut incoming) = mpsc::channel(4);
                let (_shutdown, shutdown_rx) = oneshot::channel();
                let (stop, watch) = watch::channel(false);
                let runtime = tokio::spawn(plane.run(transport, rx, events, shutdown_rx, watch));
                commands
                    .send(TunnelCommand::Ip(Zeroizing::new(outgoing)))
                    .await
                    .unwrap();
                assert!(matches!(incoming.recv().await.unwrap(), TunnelEvent::Ip(_)));
                assert!(matches!(
                    incoming.recv().await.unwrap(),
                    TunnelEvent::UnauthenticatedIke(_)
                ));
                if close_control {
                    release.send(()).unwrap();
                    server.await.unwrap();
                    assert!(matches!(
                        runtime.await.unwrap(),
                        Err(ChildDataError::Transport(DatagramTransportError::Relay(
                            crate::RelayError::ControlClosed
                        )))
                    ));
                } else {
                    stop.send(true).unwrap();
                    let stats = runtime.await.unwrap().unwrap();
                    assert_eq!(
                        probe.snapshot(),
                        TunnelTrafficSnapshot {
                            sent_esp: 1,
                            received_esp: 3,
                            accepted_ip: 1,
                            rejected_esp: 2,
                            received_ike: 1,
                            received_keepalive: 1,
                            invalid_datagram: 1,
                        }
                    );
                    assert_eq!(
                        (
                            stats.received_ip,
                            stats.received_ike,
                            stats.discarded_packets
                        ),
                        (1, 1, 3)
                    );
                    release.send(()).unwrap();
                    server.await.unwrap();
                }
            })
            .await
            .unwrap();
        }
    }
    #[tokio::test]
    async fn runtime_preserves_ike_events_discards_malformed_and_cancels() {
        use tokio::{
            net::UdpSocket,
            sync::{mpsc, oneshot, watch},
            time::{Duration, timeout},
        };
        for backpressure in [false, true] {
            timeout(Duration::from_secs(3), async {
                let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                let address = peer.local_addr().unwrap();
                let transport = EpdgDatagramTransport::open(
                    crate::ProxyMode::Direct(address.ip()),
                    "unused.invalid",
                    address.port(),
                    None,
                )
                .await
                .unwrap();
                let plane = ChildDataPlane {
                    old_receiver: None,
                    sender: EspSender::new(1, Zeroizing::new(vec![1; 20])).unwrap(),
                    receiver: EspReceiver::new(2, Zeroizing::new(vec![2; 20])).unwrap(),
                    selectors: ChildTrafficSelectors {
                        initiator: vec![],
                        responder: vec![],
                    },
                    configuration: TunnelConfiguration::default(),
                    traffic: TunnelTrafficProbe::default(),
                };
                let (commands, rx) = mpsc::channel(1);
                let (events, mut incoming) = mpsc::channel(1);
                let (_shutdown, shutdown_rx) = oneshot::channel();
                let (stop, watch) = watch::channel(false);
                let task = tokio::spawn(plane.run(transport, rx, events, shutdown_rx, watch));
                let header = proletarius_ike::IkeHeader {
                    initiator_spi: 1,
                    responder_spi: 2,
                    next_payload: 0,
                    version: 0x20,
                    exchange: proletarius_ike::ExchangeType::Informational,
                    flags: proletarius_ike::IkeHeader::RESPONSE_FLAG,
                    message_id: 3,
                };
                let ike = proletarius_ike::sk::seal_ike_sk_gcm(header, 20, &[3; 20], &[]).unwrap();
                commands
                    .send(TunnelCommand::EncryptedIke(ike.clone()))
                    .await
                    .unwrap();
                let mut buffer = [0; 128];
                let (size, source) = peer.recv_from(&mut buffer).await.unwrap();
                assert_eq!(&buffer[..size], ike.as_slice());
                peer.send_to(&[1], source).await.unwrap();
                peer.send_to(&ike, source).await.unwrap();
                if backpressure {
                    peer.send_to(&ike, source).await.unwrap();
                    assert!(matches!(
                        task.await.unwrap(),
                        Err(ChildDataError::EventConsumerUnavailable)
                    ));
                } else {
                    match incoming.recv().await.unwrap() {
                        TunnelEvent::UnauthenticatedIke(packet) => assert_eq!(packet, ike),
                        _ => panic!("control packet misclassified"),
                    }
                    stop.send(true).unwrap();
                    let stats = task.await.unwrap().unwrap();
                    assert_eq!(stats.received_ike, 1);
                    assert_eq!(stats.discarded_packets, 1);
                }
            })
            .await
            .unwrap();
        }
    }
    #[test]
    fn policy_matches_protocol_port_address_and_family() {
        let allowed: IpAddr = "10.0.0.9".parse().unwrap();
        let selectors = vec![TrafficSelector {
            protocol: 17,
            start_port: 5060,
            end_port: 5060,
            start_address: allowed,
            end_address: allowed,
        }];
        assert!(matches_selector(&selectors, allowed, 17, Some(5060)));
        assert!(!matches_selector(&selectors, allowed, 6, Some(5060)));
        assert!(!matches_selector(&selectors, allowed, 17, Some(5061)));
        assert!(!matches_selector(&selectors, allowed, 17, None));
        assert!(!matches_selector(
            &selectors,
            "10.0.0.10".parse().unwrap(),
            17,
            Some(5060)
        ));
        assert!(!matches_selector(
            &selectors,
            "::a00:9".parse().unwrap(),
            17,
            Some(5060)
        ));
    }
}
