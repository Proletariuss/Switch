//! An explicit DNS-over-SOCKS UDP diagnostic. It does not run during transport
//! selection, use a built-in public destination, or fall back to direct UDP.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use proletarius_vowifi::{
    NO_AUTH_GREETING, SocksTarget, UDP_ASSOCIATE_REQUEST, decode_udp_datagram,
    parse_no_auth_response, parse_udp_associate_response,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket, lookup_host};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout_at};

#[derive(Deserialize)]
pub(super) struct DnsProbeInput {
    server: String,
    name: String,
}

pub(super) struct DnsProbeTarget {
    server: SocketAddr,
    name: String,
}

impl DnsProbeInput {
    pub(super) fn validate(self) -> Result<DnsProbeTarget, &'static str> {
        let server = self
            .server
            .trim()
            .parse::<SocketAddr>()
            .map_err(|_| "invalid_udp_dns_server")?;
        if server.port() == 0 || server.ip().is_unspecified() || server.ip().is_multicast() {
            return Err("invalid_udp_dns_server");
        }
        let trimmed_name = self.name.trim();
        let name = trimmed_name
            .strip_suffix('.')
            .unwrap_or(trimmed_name)
            .to_ascii_lowercase();
        if name.is_empty()
            || name.len() > 253
            || name.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return Err("invalid_udp_dns_name");
        }
        Ok(DnsProbeTarget { server, name })
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct DiagnosticResult {
    ok: bool,
    tcp_connected: bool,
    authenticated: bool,
    udp_associate: bool,
    udp_roundtrip: bool,
    // Compatibility field: a successful Associate never sets this to true.
    udp_ready: bool,
    scope: &'static str,
    stage: &'static str,
    latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    roundtrip_latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dns_response_code: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<&'static str>,
}

impl DiagnosticResult {
    fn new() -> Self {
        Self {
            ok: false,
            tcp_connected: false,
            authenticated: false,
            udp_associate: false,
            udp_roundtrip: false,
            udp_ready: false,
            scope: "proxy_udp_dns",
            stage: "proxy_resolution",
            latency_ms: 0,
            roundtrip_latency_ms: None,
            dns_response_code: None,
            error_code: None,
        }
    }

    fn rank(&self) -> u8 {
        u8::from(self.tcp_connected)
            + u8::from(self.authenticated)
            + u8::from(self.udp_associate)
            + u8::from(self.udp_roundtrip)
    }

    fn failed(mut self, code: &'static str, started: Instant) -> Self {
        self.error_code = Some(code);
        self.latency_ms = elapsed_ms(started);
        self
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn finish_diagnostic(mut result: DiagnosticResult, started: Instant) -> DiagnosticResult {
    result.latency_ms = elapsed_ms(started);
    result
}

pub(super) async fn diagnose(
    proxy: &str,
    target: DnsProbeTarget,
    budget: Duration,
) -> DiagnosticResult {
    let started = Instant::now();
    let deadline = started + budget;
    let proxy = proxy
        .trim()
        .strip_prefix("socks5://")
        .or_else(|| proxy.trim().strip_prefix("socks5h://"))
        .unwrap_or(proxy.trim());
    let initial = DiagnosticResult::new();
    if budget.is_zero() {
        return initial.failed("proxy_resolution_timeout", started);
    }
    if proxy.is_empty() || proxy.len() > 512 {
        return initial.failed("invalid_proxy_address", started);
    }
    let addresses = match timeout_at(deadline, super::resolve_candidates(proxy)).await {
        Ok(Ok(addresses)) => addresses,
        Ok(Err(())) => return initial.failed("proxy_address_unavailable", started),
        Err(_) => return initial.failed("proxy_resolution_timeout", started),
    };
    let mut candidates = JoinSet::new();
    for address in addresses {
        let target = DnsProbeTarget {
            server: target.server,
            name: target.name.clone(),
        };
        candidates.spawn(diagnose_candidate(address, target, deadline));
    }
    // Dropping a cancelled diagnostic drops JoinSet and aborts all candidate
    // tasks; their control connections and UDP sockets are released with them.
    let mut best = initial;
    while let Some(candidate) = candidates.join_next().await {
        if let Ok(result) = candidate {
            if result.udp_roundtrip {
                candidates.abort_all();
                return finish_diagnostic(result, started);
            }
            if result.rank() >= best.rank() {
                best = result;
            }
        }
    }
    if best.error_code.is_none() {
        best = best.failed("proxy_probe_failed", started);
    }
    finish_diagnostic(best, started)
}

async fn diagnose_candidate(
    proxy: SocketAddr,
    target: DnsProbeTarget,
    deadline: Instant,
) -> DiagnosticResult {
    let started = Instant::now();
    let mut result = DiagnosticResult::new();
    result.stage = "tcp_connect";
    let mut control = match timeout_at(deadline, TcpStream::connect(proxy)).await {
        Ok(Ok(control)) => control,
        Ok(Err(_)) => return result.failed("proxy_tcp_failed", started),
        Err(_) => return result.failed("proxy_tcp_timeout", started),
    };
    result.tcp_connected = true;
    result.stage = "proxy_auth";
    let auth = async {
        control
            .write_all(&NO_AUTH_GREETING)
            .await
            .map_err(|_| "proxy_auth_failed")?;
        let mut response = [0; 2];
        control
            .read_exact(&mut response)
            .await
            .map_err(|_| "proxy_auth_failed")?;
        parse_no_auth_response(&response).map_err(|_| "proxy_auth_rejected")
    };
    match timeout_at(deadline, auth).await {
        Ok(Ok(())) => {}
        Ok(Err(code)) => return result.failed(code, started),
        Err(_) => return result.failed("proxy_auth_timeout", started),
    }
    result.authenticated = true;
    result.stage = "udp_associate";
    let association = async {
        control
            .write_all(&UDP_ASSOCIATE_REQUEST)
            .await
            .map_err(|_| "udp_associate_failed")?;
        let response = read_associate_response(&mut control).await?;
        let (target, port) =
            parse_udp_associate_response(&response).map_err(|_| "udp_associate_rejected")?;
        if port == 0 {
            return Err("udp_relay_unavailable");
        }
        match target {
            SocksTarget::Ip(ip) => Ok(SocketAddr::new(
                if ip.is_unspecified() { proxy.ip() } else { ip },
                port,
            )),
            SocksTarget::Domain(name) => lookup_host((name.as_str(), port))
                .await
                .map_err(|_| "udp_relay_unavailable")?
                .find(|address| address.is_ipv4() == proxy.is_ipv4())
                .ok_or("udp_relay_unavailable"),
        }
    };
    let relay = match timeout_at(deadline, association).await {
        Ok(Ok(relay)) => relay,
        Ok(Err(code)) => return result.failed(code, started),
        Err(_) => return result.failed("udp_associate_timeout", started),
    };
    result.udp_associate = true;
    result.stage = "udp_roundtrip";
    let udp = match UdpSocket::bind(match relay {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    })
    .await
    {
        Ok(socket) => socket,
        Err(_) => return result.failed("udp_socket_failed", started),
    };
    if udp.connect(relay).await.is_err() {
        return result.failed("udp_socket_failed", started);
    }
    let mut random_id = [0_u8; 2];
    if getrandom::fill(&mut random_id).is_err() {
        return result.failed("probe_identity_unavailable", started);
    }
    let query = dns_query(&target.name, random_id);
    let request = encode_udp_ip(target.server, &query);
    let roundtrip_started = Instant::now();
    let exchange = async {
        udp.send(&request).await.map_err(|_| "udp_send_failed")?;
        let mut packet = vec![0; 65_535];
        loop {
            let length = tokio::select! {
                biased;
                readiness = control.readable() => {
                    readiness.map_err(|_| "proxy_control_closed")?;
                    let mut byte = [0; 1];
                    match control.try_read(&mut byte) {
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                        _ => return Err("proxy_control_closed"),
                    }
                },
                received = udp.recv(&mut packet) => received.map_err(|_| "udp_receive_failed")?,
            };
            let response =
                decode_udp_datagram(&packet[..length]).map_err(|_| "udp_response_invalid")?;
            if response.target != SocksTarget::Ip(target.server.ip())
                || response.port != target.server.port()
            {
                return Err("udp_response_source_mismatch");
            }
            let code = validate_dns_response(&response.payload, &target.name, random_id)?;
            // An EOF already visible on the control socket invalidates its
            // association even if a UDP packet was queued at the same time.
            let mut byte = [0; 1];
            match control.try_read(&mut byte) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(code),
                _ => return Err("proxy_control_closed"),
            }
        }
    };
    let code = match timeout_at(deadline, exchange).await {
        Ok(Ok(code)) => code,
        Ok(Err(code)) => return result.failed(code, started),
        Err(_) => return result.failed("udp_roundtrip_timeout", started),
    };
    result.ok = true;
    result.udp_roundtrip = true;
    result.udp_ready = true;
    result.stage = "udp_roundtrip_confirmed";
    result.latency_ms = elapsed_ms(started);
    result.roundtrip_latency_ms = Some(elapsed_ms(roundtrip_started));
    result.dns_response_code = Some(code);
    result
}

async fn read_associate_response(control: &mut TcpStream) -> Result<Vec<u8>, &'static str> {
    let mut header = [0; 4];
    control
        .read_exact(&mut header)
        .await
        .map_err(|_| "udp_associate_failed")?;
    let mut response = header.to_vec();
    let length = match header[3] {
        1 => 6,
        4 => 18,
        3 => {
            let length = control
                .read_u8()
                .await
                .map_err(|_| "udp_associate_failed")?;
            response.push(length);
            usize::from(length) + 2
        }
        _ => return Err("udp_associate_invalid"),
    };
    let start = response.len();
    response.resize(start + length, 0);
    control
        .read_exact(&mut response[start..])
        .await
        .map_err(|_| "udp_associate_failed")?;
    Ok(response)
}

fn encode_udp_ip(target: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let mut packet = vec![0, 0, 0];
    match target.ip() {
        IpAddr::V4(ip) => {
            packet.push(1);
            packet.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            packet.push(4);
            packet.extend_from_slice(&ip.octets());
        }
    }
    packet.extend_from_slice(&target.port().to_be_bytes());
    packet.extend_from_slice(payload);
    packet
}

fn dns_query(name: &str, id: [u8; 2]) -> Vec<u8> {
    let mut query = vec![id[0], id[1], 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        query.push(u8::try_from(label.len()).expect("validated DNS label"));
        query.extend_from_slice(label.as_bytes());
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]); // A / IN
    query
}

fn validate_dns_response(packet: &[u8], name: &str, id: [u8; 2]) -> Result<u8, &'static str> {
    if packet.len() < 12 {
        return Err("dns_response_truncated");
    }
    if packet[..2] != id {
        return Err("dns_response_identity_mismatch");
    }
    // AD/CD are valid modern DNS flags; only the still-reserved Z bit is rejected.
    if packet[2] & 0x80 == 0 || packet[2] & 0x78 != 0 || packet[3] & 0x40 != 0 {
        return Err("dns_response_invalid");
    }
    if packet[2] & 2 != 0 {
        return Err("dns_response_truncated");
    }
    if packet[4..6] != [0, 1] {
        return Err("dns_response_identity_mismatch");
    }
    let (question, mut offset) = dns_name(packet, 12)?;
    if question != name || packet.get(offset..offset + 4) != Some(&[0, 1, 0, 1]) {
        return Err("dns_response_identity_mismatch");
    }
    offset += 4;
    let records = [6, 8, 10]
        .into_iter()
        .map(|index| usize::from(u16::from_be_bytes([packet[index], packet[index + 1]])))
        .sum::<usize>();
    for _ in 0..records {
        let (_, after_name) = dns_name(packet, offset)?;
        let header = packet
            .get(after_name..after_name + 10)
            .ok_or("dns_response_truncated")?;
        let data_length = usize::from(u16::from_be_bytes([header[8], header[9]]));
        offset = after_name + 10 + data_length;
        if offset > packet.len() {
            return Err("dns_response_truncated");
        }
    }
    if offset != packet.len() {
        return Err("dns_response_invalid");
    }
    Ok(packet[3] & 0x0f)
}

fn dns_name(packet: &[u8], start: usize) -> Result<(String, usize), &'static str> {
    let mut offset = start;
    let mut consumed = None;
    let mut labels = Vec::new();
    // Every pointer must move backwards, and names/pointer chains are bounded.
    for _ in 0..128 {
        let length = *packet.get(offset).ok_or("dns_response_truncated")?;
        if length == 0 {
            return Ok((labels.join("."), consumed.unwrap_or(offset + 1)));
        }
        if length & 0xc0 == 0xc0 {
            let low = *packet.get(offset + 1).ok_or("dns_response_truncated")?;
            let next = (usize::from(length & 0x3f) << 8) | usize::from(low);
            if next >= offset {
                return Err("dns_response_invalid");
            }
            consumed.get_or_insert(offset + 2);
            offset = next;
            continue;
        }
        if length > 63 {
            return Err("dns_response_invalid");
        }
        let bytes = packet
            .get(offset + 1..offset + 1 + usize::from(length))
            .ok_or("dns_response_truncated")?;
        let label = std::str::from_utf8(bytes).map_err(|_| "dns_response_invalid")?;
        if !label.is_ascii() {
            return Err("dns_response_invalid");
        }
        labels.push(label.to_ascii_lowercase());
        if labels.iter().map(String::len).sum::<usize>() + labels.len() > 254 {
            return Err("dns_response_invalid");
        }
        offset += 1 + usize::from(length);
    }
    Err("dns_response_invalid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    #[derive(Clone, Copy)]
    enum Reply {
        Valid,
        Blackhole,
        WrongSource,
        WrongID,
        WrongQuestion,
        Truncated,
        TC,
        ControlClosed,
        OuterWrongSource,
        AuthRejected,
        AssociateRejected,
        AuthBlackhole,
        AssociateBlackhole,
    }

    struct Fixture {
        address: SocketAddr,
        associated: oneshot::Receiver<()>,
        closed: oneshot::Receiver<()>,
        task: tokio::task::JoinHandle<()>,
    }

    async fn fixture(ipv6: bool, wildcard: bool, mode: Reply) -> Fixture {
        let host = if ipv6 { "[::1]:0" } else { "127.0.0.1:0" };
        let tcp = TcpListener::bind(host).await.expect("loopback listener");
        let address = tcp.local_addr().unwrap();
        let udp = UdpSocket::bind(host).await.unwrap();
        let relay = udp.local_addr().unwrap();
        let (associated_send, associated) = oneshot::channel();
        let (closed_send, closed) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut control, _) = tcp.accept().await.unwrap();
            let mut greeting = [0; 3];
            control.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, NO_AUTH_GREETING);
            if matches!(mode, Reply::AuthBlackhole) {
                let mut byte = [0; 1];
                assert_eq!(control.read(&mut byte).await.unwrap(), 0);
                let _ = closed_send.send(());
                return;
            }
            if matches!(mode, Reply::AuthRejected) {
                control.write_all(&[5, 2]).await.unwrap();
                return;
            }
            control.write_all(&[5, 0]).await.unwrap();
            let mut request = [0; 10];
            control.read_exact(&mut request).await.unwrap();
            assert_eq!(request, UDP_ASSOCIATE_REQUEST);
            if matches!(mode, Reply::AssociateBlackhole) {
                let mut byte = [0; 1];
                assert_eq!(control.read(&mut byte).await.unwrap(), 0);
                let _ = closed_send.send(());
                return;
            }
            let bind = if wildcard {
                SocketAddr::new(
                    if ipv6 {
                        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
                    } else {
                        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                    },
                    relay.port(),
                )
            } else {
                relay
            };
            let mut reply = encode_udp_ip(bind, &[]);
            reply[0] = 5;
            reply[1] = if matches!(mode, Reply::AssociateRejected) {
                7
            } else {
                0
            };
            control.write_all(&reply).await.unwrap();
            if matches!(mode, Reply::AssociateRejected) {
                return;
            }
            let mut packet = [0; 1_024];
            let (length, client) = udp.recv_from(&mut packet).await.unwrap();
            let _ = associated_send.send(());
            let request = decode_udp_datagram(&packet[..length]).unwrap();
            assert_eq!(request.port, 53);
            let SocksTarget::Ip(server_ip) = request.target else {
                panic!("destination was resolved or changed");
            };
            let server = SocketAddr::new(server_ip, request.port);
            if matches!(mode, Reply::ControlClosed) {
                drop(control);
                let _ = closed_send.send(());
                tokio::time::sleep(Duration::from_millis(10)).await;
                let mut response = request.payload;
                response[2] |= 0x80;
                let _ = udp.send_to(&encode_udp_ip(server, &response), client).await;
                return;
            }
            if !matches!(mode, Reply::Blackhole) {
                let mut response = request.payload;
                response[2] |= 0x80;
                let mut source = server;
                match mode {
                    Reply::WrongSource => source.set_port(54),
                    Reply::WrongID => response[0] ^= 1,
                    Reply::WrongQuestion => response[13] = b'x',
                    Reply::Truncated => {
                        response.pop();
                    }
                    Reply::TC => response[2] |= 2,
                    _ => {}
                }
                let response = encode_udp_ip(source, &response);
                if matches!(mode, Reply::OuterWrongSource) {
                    let rogue = UdpSocket::bind(host).await.unwrap();
                    rogue.send_to(&response, client).await.unwrap();
                } else {
                    udp.send_to(&response, client).await.unwrap();
                }
            }
            let mut byte = [0; 1];
            assert_eq!(control.read(&mut byte).await.unwrap(), 0);
            let _ = closed_send.send(());
        });
        Fixture {
            address,
            associated,
            closed,
            task,
        }
    }

    fn target(ipv6: bool) -> DnsProbeTarget {
        DnsProbeTarget {
            server: if ipv6 {
                "[2001:db8::53]:53"
            } else {
                "192.0.2.53:53"
            }
            .parse()
            .unwrap(),
            name: "probe.invalid".into(),
        }
    }

    #[tokio::test]
    async fn confirms_only_explicit_dns_roundtrip_for_ipv4_ipv6_and_wildcard_relays() {
        for (ipv6, wildcard) in [(false, false), (false, true), (true, false), (true, true)] {
            let fixture = fixture(ipv6, wildcard, Reply::Valid).await;
            let result = diagnose(
                &fixture.address.to_string(),
                target(ipv6),
                Duration::from_millis(500),
            )
            .await;
            assert!(
                result.ok
                    && result.tcp_connected
                    && result.authenticated
                    && result.udp_associate
                    && result.udp_roundtrip
                    && result.udp_ready,
                "{result:?}"
            );
            assert_eq!(result.scope, "proxy_udp_dns");
            assert_eq!(result.dns_response_code, Some(0));
            fixture.task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn associate_success_with_udp_blackhole_is_not_ready_and_keeps_total_deadline() {
        let fixture = fixture(false, false, Reply::Blackhole).await;
        let started = Instant::now();
        let result = diagnose(
            &fixture.address.to_string(),
            target(false),
            Duration::from_millis(50),
        )
        .await;
        assert!(result.tcp_connected && result.authenticated && result.udp_associate);
        assert!(!result.ok && !result.udp_roundtrip && !result.udp_ready);
        assert_eq!(result.error_code, Some("udp_roundtrip_timeout"));
        assert!(started.elapsed() < Duration::from_millis(250));
        fixture.task.await.unwrap();
    }

    #[tokio::test]
    async fn unverified_source_id_question_and_truncated_packets_never_report_ready() {
        for (mode, error) in [
            (Reply::WrongSource, "udp_response_source_mismatch"),
            (Reply::WrongID, "dns_response_identity_mismatch"),
            (Reply::WrongQuestion, "dns_response_identity_mismatch"),
            (Reply::Truncated, "dns_response_identity_mismatch"),
            (Reply::TC, "dns_response_truncated"),
            (Reply::OuterWrongSource, "udp_roundtrip_timeout"),
        ] {
            let fixture = fixture(false, false, mode).await;
            let result = diagnose(
                &fixture.address.to_string(),
                target(false),
                Duration::from_millis(80),
            )
            .await;
            assert!(
                result.udp_associate && !result.udp_roundtrip && !result.udp_ready,
                "{result:?}"
            );
            assert_eq!(result.error_code, Some(error));
            fixture.task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn closed_control_connection_invalidates_a_queued_udp_response() {
        let fixture = fixture(false, false, Reply::ControlClosed).await;
        let result = diagnose(
            &fixture.address.to_string(),
            target(false),
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(result.error_code, Some("proxy_control_closed"));
        assert!(!result.udp_ready);
        fixture.task.await.unwrap();
    }

    #[tokio::test]
    async fn cancellation_drops_control_and_udp_without_waiting_for_the_deadline() {
        let fixture = fixture(false, false, Reply::Blackhole).await;
        let address = fixture.address.to_string();
        let diagnostic =
            tokio::spawn(
                async move { diagnose(&address, target(false), Duration::from_secs(5)).await },
            );
        fixture.associated.await.unwrap();
        diagnostic.abort();
        assert!(diagnostic.await.unwrap_err().is_cancelled());
        timeout_at(Instant::now() + Duration::from_millis(250), fixture.closed)
            .await
            .expect("control connection leaked")
            .unwrap();
        fixture.task.await.unwrap();
    }

    #[tokio::test]
    async fn reports_auth_and_associate_failures_as_different_evidence() {
        for (mode, error, authenticated) in [
            (Reply::AuthRejected, "proxy_auth_rejected", false),
            (Reply::AssociateRejected, "udp_associate_rejected", true),
        ] {
            let fixture = fixture(false, false, mode).await;
            let result = diagnose(
                &fixture.address.to_string(),
                target(false),
                Duration::from_millis(250),
            )
            .await;
            assert!(result.tcp_connected);
            assert_eq!(result.authenticated, authenticated);
            assert!(!result.udp_associate && !result.udp_roundtrip);
            assert_eq!(result.error_code, Some(error));
            fixture.task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn auth_and_associate_share_the_total_deadline_and_release_connections() {
        for (mode, error, authenticated) in [
            (Reply::AuthBlackhole, "proxy_auth_timeout", false),
            (Reply::AssociateBlackhole, "udp_associate_timeout", true),
        ] {
            let fixture = fixture(false, false, mode).await;
            let started = Instant::now();
            let result = diagnose(
                &fixture.address.to_string(),
                target(false),
                Duration::from_millis(40),
            )
            .await;
            assert!(result.tcp_connected && !result.udp_associate && !result.udp_ready);
            assert_eq!(result.authenticated, authenticated);
            assert_eq!(result.error_code, Some(error));
            assert!(started.elapsed() < Duration::from_millis(250));
            fixture.task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn zero_budget_never_connects_to_the_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let result = diagnose(
            &listener.local_addr().unwrap().to_string(),
            target(false),
            Duration::ZERO,
        )
        .await;
        assert_eq!(result.error_code, Some("proxy_resolution_timeout"));
        assert!(!result.tcp_connected);
        assert!(
            timeout_at(
                Instant::now() + Duration::from_millis(20),
                listener.accept()
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn requires_explicit_literal_destination_and_valid_name() {
        for server in [
            "",
            "dns.example:53",
            "127.0.0.1:0",
            "0.0.0.0:53",
            "224.0.0.1:53",
            "[::]:53",
        ] {
            assert!(
                DnsProbeInput {
                    server: server.into(),
                    name: "probe.invalid".into()
                }
                .validate()
                .is_err()
            );
        }
        for name in [
            "",
            ".",
            "a..",
            "a..b",
            "-a.example",
            "a-.example",
            "a b.example",
            "é.example",
        ] {
            assert!(
                DnsProbeInput {
                    server: "127.0.0.1:53".into(),
                    name: name.into()
                }
                .validate()
                .is_err()
            );
        }
        assert_eq!(
            DnsProbeInput {
                server: "[::1]:53".into(),
                name: "Probe.Invalid.".into()
            }
            .validate()
            .unwrap()
            .name,
            "probe.invalid"
        );
    }

    #[test]
    fn negative_dns_response_is_roundtrip_evidence_but_not_a_resolution_success() {
        let mut response = dns_query("probe.invalid", [12, 34]);
        response[2] |= 0x80;
        response[3] = 3;
        assert_eq!(
            validate_dns_response(&response, "probe.invalid", [12, 34]),
            Ok(3)
        );
        response[3] = 0x30; // AD and CD are valid, not reserved Z bits.
        assert_eq!(
            validate_dns_response(&response, "probe.invalid", [12, 34]),
            Ok(0)
        );
    }

    #[test]
    fn dns_structure_rejects_short_header_missing_records_pointer_loops_and_trailing_data() {
        assert_eq!(
            validate_dns_response(&[0; 8], "probe.invalid", [0, 0]),
            Err("dns_response_truncated")
        );
        let mut response = dns_query("probe.invalid", [12, 34]);
        response[2] |= 0x80;
        response[7] = 1;
        assert!(validate_dns_response(&response, "probe.invalid", [12, 34]).is_err());
        response[7] = 0;
        response.push(0);
        assert_eq!(
            validate_dns_response(&response, "probe.invalid", [12, 34]),
            Err("dns_response_invalid")
        );
        let packet = [0xc0, 0];
        assert_eq!(dns_name(&packet, 0), Err("dns_response_invalid"));
    }

    #[test]
    fn validates_complete_compressed_dns_answers_and_rejects_missing_rdata() {
        let mut response = dns_query("probe.invalid", [12, 34]);
        response[2] |= 0x80;
        response[7] = 1;
        response.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4, 192, 0, 2, 1]);
        assert_eq!(
            validate_dns_response(&response, "probe.invalid", [12, 34]),
            Ok(0)
        );
        response.pop();
        assert_eq!(
            validate_dns_response(&response, "probe.invalid", [12, 34]),
            Err("dns_response_truncated")
        );
    }

    #[test]
    fn total_latency_includes_resolution_and_candidate_wait_but_preserves_udp_segment() {
        let mut candidate = DiagnosticResult::new();
        candidate.latency_ms = 1;
        candidate.roundtrip_latency_ms = Some(1);
        let completed = finish_diagnostic(candidate, Instant::now() - Duration::from_millis(50));
        assert!(completed.latency_ms >= 50);
        assert_eq!(completed.roundtrip_latency_ms, Some(1));
    }
}
