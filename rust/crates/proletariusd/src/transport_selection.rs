//! Resolves and selects the ePDG transport inside the Rust control plane.
//! The compatibility process receives only the selected mode and destination;
//! it no longer duplicates DNS, route, fake-IP, or SOCKS5 policy decisions.

use std::collections::HashSet;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use crate::system_proxy::StartupCommandControl;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Incoming};
use proletarius_vowifi::{
    ProxyEvidence, ProxyMode, ProxyPolicyError, RouteEvidence, select_proxy_mode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::lookup_host;
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};

const BODY_LIMIT: usize = 8_192;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
const ROUTE_TIMEOUT: Duration = Duration::from_secs(3);
const SURGE_DNS_OUTPUT_LIMIT: usize = 16 * 1024;
const ROUTE_OUTPUT_LIMIT: usize = 64 * 1024;
const SURGE_CLI: &str = "/Applications/Surge.app/Contents/Applications/surge-cli";
// Explicit SOCKS probes keep their six-second budget. This outer bound leaves
// a small Unix-socket/JSON margin below Go's seven-second startup context;
// automatic system discovery has its own shorter budget in system_proxy.
const TRANSPORT_SELECTION_BUDGET: Duration = Duration::from_millis(6_800);

#[derive(Deserialize)]
struct SelectionInput {
    #[serde(default)]
    proxy: String,
    epdg: String,
}

/// Immutable transport decision consumed by the future native session owner.
/// `proxy` is present only after a real SOCKS handshake resolved the endpoint;
/// transparent/direct modes never need to retain it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectedTransport {
    pub mode: ProxyMode,
    pub proxy: Option<SocketAddr>,
    pub proxy_error: Option<&'static str>,
    pub proxy_source: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionSelectionError {
    InvalidEpdg,
    TimedOut,
    Policy {
        error: ProxyPolicyError,
        proxy_error: Option<&'static str>,
        proxy_source: Option<&'static str>,
    },
    MissingVerifiedProxy,
}

struct TransportDecision {
    selection: Result<ProxyMode, ProxyPolicyError>,
    proxy: Option<SocketAddr>,
    proxy_error: Option<&'static str>,
    proxy_source: Option<&'static str>,
}

pub(crate) async fn handle(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        BODY_LIMIT,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(value) => value,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return failure(StatusCode::BAD_REQUEST, "invalid_selection_request"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
    };
    let Ok(input) = serde_json::from_slice::<SelectionInput>(&body) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_selection_request");
    };
    if !valid_hostname(&input.epdg) {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_epdg_hostname");
    }

    match select_for_session(input.proxy.trim(), input.epdg.trim()).await {
        Ok(selected) => {
            let proxy_binding_valid =
                matches!(selected.mode, ProxyMode::Socks5UdpDomain) == selected.proxy.is_some();
            if !proxy_binding_valid {
                return failure(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "transport_binding_invalid",
                );
            }
            selection_response(
                Ok(selected.mode),
                selected.proxy,
                selected.proxy_error,
                selected.proxy_source,
            )
        }
        Err(SessionSelectionError::Policy {
            error,
            proxy_error,
            proxy_source,
        }) => selection_response(Err(error), None, proxy_error, proxy_source),
        Err(SessionSelectionError::InvalidEpdg) => {
            failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_epdg_hostname")
        }
        Err(SessionSelectionError::TimedOut) => crate::json_response(
            StatusCode::OK,
            &json!({"mode":"rejected","error_code":"transport_selection_timeout"}),
        ),
        Err(SessionSelectionError::MissingVerifiedProxy) => selection_response(
            Err(ProxyPolicyError::Socks5UdpUnavailable),
            None,
            Some("proxy_address_unavailable"),
            None,
        ),
    }
}

#[cfg(test)]
async fn select_transport(proxy: &str, epdg: &str) -> Response<Full<Bytes>> {
    let decision = decide_transport_with_source(proxy, epdg, None).await;
    selection_response(
        decision.selection,
        decision.proxy,
        decision.proxy_error,
        decision.proxy_source,
    )
}

pub(crate) async fn select_for_session(
    requested_proxy: &str,
    epdg: &str,
) -> Result<SelectedTransport, SessionSelectionError> {
    select_for_session_with_stop(requested_proxy, epdg, None, None).await
}

pub(crate) async fn select_for_session_with_stop(
    requested_proxy: &str,
    epdg: &str,
    stop: Option<tokio::sync::watch::Receiver<bool>>,
    outer_deadline: Option<Instant>,
) -> Result<SelectedTransport, SessionSelectionError> {
    let mut control = StartupCommandControl::new(TRANSPORT_SELECTION_BUDGET, stop);
    if let Some(deadline) = outer_deadline {
        control = control.with_deadline(deadline);
    }
    let result = select_for_session_unbounded(requested_proxy, epdg, &control).await;
    if control.is_open() {
        result
    } else {
        Err(SessionSelectionError::TimedOut)
    }
}

fn remaining_selection_budget(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

async fn select_for_session_unbounded(
    requested_proxy: &str,
    epdg: &str,
    control: &StartupCommandControl,
) -> Result<SelectedTransport, SessionSelectionError> {
    if !valid_hostname(epdg) {
        return Err(SessionSelectionError::InvalidEpdg);
    }
    let decision = decide_transport(requested_proxy.trim(), epdg.trim(), control).await;
    let mode = decision
        .selection
        .map_err(|error| SessionSelectionError::Policy {
            error,
            proxy_error: decision.proxy_error,
            proxy_source: decision.proxy_source,
        })?;
    let proxy = match mode {
        ProxyMode::Socks5UdpDomain => Some(
            decision
                .proxy
                .ok_or(SessionSelectionError::MissingVerifiedProxy)?,
        ),
        _ => None,
    };
    Ok(SelectedTransport {
        mode,
        proxy,
        proxy_error: decision.proxy_error,
        proxy_source: decision.proxy_source,
    })
}

async fn decide_transport(
    requested_proxy: &str,
    epdg: &str,
    control: &StartupCommandControl,
) -> TransportDecision {
    if !control.is_open() {
        return revoked_decision();
    }
    let discovery = if requested_proxy.is_empty() {
        Some(crate::system_proxy::discover_with_control(control).await)
    } else {
        None
    };
    let enhanced_mode = if requested_proxy.is_empty()
        && discovery
            .as_ref()
            .is_some_and(|value| value.proxy.is_none() && value.diagnostic.is_none())
    {
        crate::system_proxy::observe_surge_enhanced_mode_with_control(control).await
    } else {
        crate::system_proxy::SurgeEnhancedMode::Off
    };
    if !control.is_open() {
        return revoked_decision();
    }
    decide_transport_after_discovery_controlled(
        requested_proxy,
        epdg,
        control.deadline(),
        discovery,
        enhanced_mode,
        control,
    )
    .await
}

#[cfg(test)]
async fn decide_transport_after_discovery(
    requested_proxy: &str,
    epdg: &str,
    selection_deadline: Instant,
    discovery: Option<crate::system_proxy::SystemProxyDiscovery>,
    enhanced_mode: crate::system_proxy::SurgeEnhancedMode,
) -> TransportDecision {
    let control = StartupCommandControl::new(remaining_selection_budget(selection_deadline), None);
    decide_transport_after_discovery_controlled(
        requested_proxy,
        epdg,
        selection_deadline,
        discovery,
        enhanced_mode,
        &control,
    )
    .await
}

fn revoked_decision() -> TransportDecision {
    TransportDecision {
        selection: Err(ProxyPolicyError::UnverifiedPublicRoute),
        proxy: None,
        proxy_error: None,
        proxy_source: None,
    }
}

async fn decide_transport_after_discovery_controlled(
    requested_proxy: &str,
    epdg: &str,
    selection_deadline: Instant,
    discovery: Option<crate::system_proxy::SystemProxyDiscovery>,
    enhanced_mode: crate::system_proxy::SurgeEnhancedMode,
    control: &StartupCommandControl,
) -> TransportDecision {
    if !control.is_open() {
        return revoked_decision();
    }
    if let Some(code) = discovery_blocker(discovery.as_ref()) {
        // A failed or HTTP/PAC-only system-proxy observation is not proof
        // that direct ePDG DNS or UDP is authorized. No fallback I/O runs.
        return TransportDecision {
            selection: Err(ProxyPolicyError::Socks5UdpUnavailable),
            proxy: None,
            proxy_error: Some(code),
            proxy_source: None,
        };
    }
    if enhanced_mode == crate::system_proxy::SurgeEnhancedMode::Unknown {
        return TransportDecision {
            selection: Err(ProxyPolicyError::Socks5UdpUnavailable),
            proxy: None,
            proxy_error: Some("surge_enhanced_mode_unavailable"),
            proxy_source: None,
        };
    }
    let discovered_proxy = discovery.as_ref().and_then(|result| result.proxy.as_ref());
    let effective_proxy = discovered_proxy
        .as_ref()
        .map_or(requested_proxy, |proxy| proxy.endpoint.as_str());
    let proxy_source = if requested_proxy.is_empty() {
        discovered_proxy.map(|proxy| proxy.source).or((enhanced_mode
            == crate::system_proxy::SurgeEnhancedMode::On)
            .then_some("surge-enhanced-mode"))
    } else {
        Some("explicit")
    };
    let diagnostic = discovery.as_ref().and_then(|result| result.diagnostic);
    attach_discovery_diagnostic(
        decide_transport_with_source_and_probe_budget(
            effective_proxy,
            epdg,
            proxy_source,
            probe_budget_for_selection(requested_proxy, selection_deadline),
            control,
        )
        .await,
        diagnostic,
    )
}

fn discovery_blocker(
    discovery: Option<&crate::system_proxy::SystemProxyDiscovery>,
) -> Option<&'static str> {
    discovery.filter(|value| value.proxy.is_none())?.diagnostic
}

fn probe_budget_for_selection(requested_proxy: &str, selection_deadline: Instant) -> Duration {
    if requested_proxy.is_empty() {
        remaining_selection_budget(selection_deadline).min(Duration::from_secs(6))
    } else {
        // Never shorten explicit SOCKS5 UDP verification merely because the
        // automatic System Proxy path is also bounded.
        Duration::from_secs(6)
    }
}

fn attach_discovery_diagnostic(
    mut decision: TransportDecision,
    diagnostic: Option<&'static str>,
) -> TransportDecision {
    if decision.proxy_error.is_none() {
        decision.proxy_error = diagnostic;
    }
    decision
}

#[cfg(test)]
async fn decide_transport_with_source(
    proxy: &str,
    epdg: &str,
    proxy_source: Option<&'static str>,
) -> TransportDecision {
    let control = StartupCommandControl::new(TRANSPORT_SELECTION_BUDGET, None);
    decide_transport_with_source_and_probe_budget(
        proxy,
        epdg,
        proxy_source,
        Duration::from_secs(6),
        &control,
    )
    .await
}

async fn decide_transport_with_source_and_probe_budget(
    proxy: &str,
    epdg: &str,
    proxy_source: Option<&'static str>,
    proxy_budget: Duration,
    control: &StartupCommandControl,
) -> TransportDecision {
    if !control.is_open() {
        return revoked_decision();
    }
    // A verified SOCKS5 UDP association retains the ePDG hostname as a
    // proxy-side domain target. If Surge's SOCKS listener lacks UDP, resolve
    // through Surge's own DNS configuration before checking its VIF route.
    // Never fall back to the host resolver while a front proxy is configured.
    let proxy_probe = if proxy.is_empty() {
        None
    } else {
        match control.observe(probe_proxy(proxy, proxy_budget)).await {
            Some(result) => Some(result),
            None => return revoked_decision(),
        }
    };
    if !control.is_open() {
        return revoked_decision();
    }
    decide_from_proxy_probe(
        proxy_probe,
        proxy_source,
        configured_proxy_is_loopback(proxy),
        || async {
            if !control.is_open() {
                return (Vec::new(), Vec::new());
            }
            let addresses = match dns_owner(proxy_source, !proxy.is_empty()) {
                DnsOwner::Surge => resolve_epdg_via_surge_controlled(epdg, SURGE_CLI, control)
                    .await
                    .unwrap_or_default(),
                DnsOwner::HostDirect => control
                    .observe(resolve_epdg(epdg))
                    .await
                    .and_then(Result::ok)
                    .unwrap_or_default(),
                DnsOwner::Unavailable => Vec::new(),
            };
            if !control.is_open() {
                return (addresses, Vec::new());
            }
            let routes = collect_routes_controlled(&addresses, control).await;
            (addresses, routes)
        },
    )
    .await
}

async fn decide_from_proxy_probe<F, Fut>(
    proxy_probe: Option<Result<crate::proxy_probe::EndpointProbe, &'static str>>,
    proxy_source: Option<&'static str>,
    configured_proxy_is_loopback: bool,
    resolve_evidence: F,
) -> TransportDecision
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = (Vec<IpAddr>, Vec<RouteEvidence>)>,
{
    // A TCP SOCKS handshake is not enough for IKE. Preserve an explicit
    // capability diagnostic when the listener accepts TCP but rejects UDP
    // ASSOCIATE; the transparent VIF fallback may still be usable, but it
    // must not make the configured proxy look like a verified UDP proxy.
    let proxy_error = match proxy_probe.as_ref() {
        Some(Ok(probe)) if !probe.result.udp_associate => Some("socks5_udp_associate_unavailable"),
        Some(Err(error)) => Some(*error),
        _ => None,
    };
    let proxy_address = proxy_probe
        .as_ref()
        .and_then(|result| result.as_ref().ok().map(|probe| probe.address));
    if proxy_probe.as_ref().is_some_and(|result| {
        result
            .as_ref()
            .is_ok_and(|probe| probe.result.udp_associate)
    }) {
        return TransportDecision {
            selection: Ok(ProxyMode::Socks5UdpDomain),
            proxy: proxy_address,
            proxy_error,
            proxy_source,
        };
    }

    let (addresses, routes) = resolve_evidence().await;
    // A local TUN can carry fake-IP UDP even if its HTTP/SOCKS port does not
    // support UDP ASSOCIATE. Verify the actual route before selecting fake-IP;
    // a stale fake DNS answer alone is not evidence of a working TUN.
    let verified_fake_route = routes.iter().any(|route| {
        proletarius_vowifi::is_fake_ip(route.address)
            && route
                .interface
                .as_deref()
                .is_some_and(|interface| interface.starts_with("utun"))
    });
    let proxy_is_loopback = verified_fake_route && configured_proxy_is_loopback;
    let evidence = ProxyEvidence {
        proxy_is_loopback,
        epdg_addresses: addresses,
        routes,
        socks5_udp_associate: false,
    };
    let selection = match select_proxy_mode(&evidence) {
        Ok(ProxyMode::Direct(_)) | Err(ProxyPolicyError::UnverifiedPublicRoute)
            if proxy_probe.is_some() || proxy_source == Some("surge-enhanced-mode") =>
        {
            Err(if proxy_probe.is_some() {
                ProxyPolicyError::Socks5UdpUnavailable
            } else {
                ProxyPolicyError::UnverifiedPublicRoute
            })
        }
        other => other,
    };
    TransportDecision {
        selection,
        proxy: proxy_address,
        proxy_error,
        proxy_source,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DnsOwner {
    Surge,
    HostDirect,
    Unavailable,
}

fn dns_owner(proxy_source: Option<&str>, has_proxy: bool) -> DnsOwner {
    if matches!(
        proxy_source,
        Some("surge-effective-profile" | "surge-enhanced-mode")
    ) {
        DnsOwner::Surge
    } else if has_proxy {
        DnsOwner::Unavailable
    } else {
        DnsOwner::HostDirect
    }
}

#[derive(Deserialize)]
struct SurgeDnsReply {
    domain: String,
    #[serde(rename = "v4Addresses", default)]
    ipv4: Vec<Ipv4Addr>,
    #[serde(rename = "v6Addresses", default)]
    ipv6: Vec<Ipv6Addr>,
}

fn parse_surge_dns_reply(host: &str, bytes: &[u8]) -> Result<Vec<IpAddr>, ()> {
    if bytes.len() > SURGE_DNS_OUTPUT_LIMIT {
        return Err(());
    }
    let reply: SurgeDnsReply = serde_json::from_slice(bytes).map_err(|_| ())?;
    if !reply
        .domain
        .trim_end_matches('.')
        .eq_ignore_ascii_case(host.trim_end_matches('.'))
    {
        return Err(());
    }
    let mut seen = HashSet::new();
    let addresses: Vec<_> = reply
        .ipv4
        .into_iter()
        .map(IpAddr::V4)
        .chain(reply.ipv6.into_iter().map(IpAddr::V6))
        .filter(|address| seen.insert(*address))
        .collect();
    if addresses.is_empty() {
        return Err(());
    }
    Ok(addresses)
}

async fn resolve_epdg_via_surge_controlled(
    host: &str,
    cli: &str,
    control: &StartupCommandControl,
) -> Result<Vec<IpAddr>, ()> {
    let output = crate::system_proxy::run_bounded_bytes(
        cli,
        &["--raw", "dns", "lookup", host],
        SURGE_DNS_OUTPUT_LIMIT,
        LOOKUP_TIMEOUT,
        control,
    )
    .await
    .ok_or(())?;
    parse_surge_dns_reply(host, &output)
}

fn configured_proxy_is_loopback(proxy: &str) -> bool {
    let endpoint = proxy
        .split_once("://")
        .map(|(_, endpoint)| endpoint)
        .unwrap_or(proxy);
    crate::system_proxy::is_loopback_endpoint(endpoint)
}

async fn resolve_epdg(host: &str) -> Result<Vec<IpAddr>, ()> {
    let resolved = timeout(LOOKUP_TIMEOUT, lookup_host((host, 500)))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    let mut seen = HashSet::new();
    Ok(resolved
        .map(|address| address.ip())
        .filter(|address| seen.insert(*address))
        .collect())
}

async fn probe_proxy(
    proxy: &str,
    budget: Duration,
) -> Result<crate::proxy_probe::EndpointProbe, &'static str> {
    if proxy.is_empty() {
        return Err("proxy_not_configured");
    }
    crate::proxy_probe::probe_endpoint_with_budget(proxy, budget).await
}

async fn collect_routes_controlled(
    addresses: &[IpAddr],
    control: &StartupCommandControl,
) -> Vec<RouteEvidence> {
    let control = control.clone();
    collect_routes_with(addresses, move |address| {
        let control = control.clone();
        async move { route_interface_controlled(address, "/sbin/route", &control).await }
    })
    .await
}

async fn collect_routes_with<F, Fut>(addresses: &[IpAddr], lookup: F) -> Vec<RouteEvidence>
where
    F: Fn(IpAddr) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<String>> + Send + 'static,
{
    let mut tasks = JoinSet::new();
    for (index, address) in addresses.iter().copied().enumerate() {
        let lookup = lookup.clone();
        tasks.spawn(async move { (index, address, lookup(address).await) });
    }
    let mut ordered = vec![None; addresses.len()];
    while let Some(result) = tasks.join_next().await {
        if let Ok((index, address, Some(interface))) = result {
            ordered[index] = Some(RouteEvidence {
                address,
                interface: Some(interface),
            });
        }
    }
    ordered.into_iter().flatten().collect()
}

async fn route_interface_controlled(
    address: IpAddr,
    program: &str,
    control: &StartupCommandControl,
) -> Option<String> {
    let output = crate::system_proxy::run_bounded_bytes(
        program,
        &["-n", "get", &address.to_string()],
        ROUTE_OUTPUT_LIMIT,
        ROUTE_TIMEOUT,
        control,
    )
    .await?;
    parse_route_interface(&String::from_utf8_lossy(&output))
}

fn parse_route_interface(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        match (fields.next(), fields.next(), fields.next()) {
            (Some("interface:"), Some(interface), None) if !interface.is_empty() => {
                Some(interface.to_owned())
            }
            _ => None,
        }
    })
}

pub(crate) fn valid_hostname(value: &str) -> bool {
    let value = value.trim().trim_end_matches('.');
    !value.is_empty()
        && value.len() <= 253
        && value.is_ascii()
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn selection_response(
    selection: Result<ProxyMode, proletarius_vowifi::ProxyPolicyError>,
    proxy: Option<SocketAddr>,
    proxy_error: Option<&str>,
    proxy_source: Option<&str>,
) -> Response<Full<Bytes>> {
    // A SOCKS endpoint is a capability binding, not general diagnostics. It
    // is emitted only for the one mode that needs it, after `probe_proxy`
    // completed its UDP ASSOCIATE handshake. Returning the resolved socket
    // also ensures the Go compatibility transport does not perform a second
    // DNS resolution after Rust has already verified the endpoint.
    let include_proxy_endpoint = matches!(&selection, Ok(ProxyMode::Socks5UdpDomain));
    let mut value = match selection {
        Ok(ProxyMode::EnhancedFakeIp(address)) => {
            json!({"mode":"enhanced-mode-fake-ip","address":address.to_string()})
        }
        Ok(ProxyMode::EnhancedUtun { address, interface }) => json!({
            "mode":"enhanced-mode-utun",
            "address":address.to_string(),
            "interface":interface,
        }),
        Ok(ProxyMode::Socks5UdpDomain) => json!({"mode":"socks5-udp-domain"}),
        Ok(ProxyMode::Direct(address)) => {
            json!({"mode":"direct","address":address.to_string()})
        }
        Err(error) => json!({
            "mode":"rejected",
            "error_code":match error {
                proletarius_vowifi::ProxyPolicyError::Socks5UdpUnavailable => "socks5_udp_unavailable",
                proletarius_vowifi::ProxyPolicyError::UnverifiedPublicRoute => "unverified_public_route",
            },
        }),
    };
    if let (Some(error), Value::Object(object)) = (proxy_error, &mut value) {
        object.insert("proxy_error".to_owned(), Value::String(error.to_owned()));
    }
    if let (Some(source), Value::Object(object)) = (proxy_source, &mut value) {
        object.insert("proxy_source".to_owned(), Value::String(source.to_owned()));
    }
    if include_proxy_endpoint {
        if let (Some(proxy), Value::Object(object)) = (proxy, &mut value) {
            object.insert(
                "proxy_endpoint".to_owned(),
                Value::String(proxy.to_string()),
            );
        }
    }
    crate::json_response(StatusCode::OK, &value)
}

fn failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"ePDG 传输参数无效","code":code,"retryable":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, UdpSocket};
    use tokio::sync::Barrier;

    #[tokio::test]
    async fn surge_dns_parent_deadline_joins_real_cli_before_returning() {
        use crate::system_proxy::{StartupReapAck, tests::PrivateCli};
        let cli = PrivateCli::new("echo $$ >> pids; echo dns >> trace; exec /bin/sleep 60");
        let ack = Arc::new(StartupReapAck::default());
        let mut control = StartupCommandControl::new(Duration::from_millis(150), None);
        control.reap_ack = Some(ack.clone());
        let path = cli.path.clone();
        let task = tokio::spawn(async move {
            resolve_epdg_via_surge_controlled("epdg.invalid", &path, &control).await
        });
        cli.pids(1).await;
        tokio::time::timeout(Duration::from_secs(3), ack.entered.notified())
            .await
            .unwrap();
        assert!(
            !task.is_finished(),
            "DNS must retain the actual CLI reap future"
        );
        ack.release.add_permits(1);
        assert!(
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        cli.assert_reaped(1).await;
        assert_eq!(cli.trace().lines().count(), 1);
    }

    #[tokio::test]
    async fn cancelled_route_joinset_drains_all_real_children_before_returning() {
        use crate::system_proxy::{StartupReapAck, tests::PrivateCli};
        let cli = PrivateCli::new("echo $$ >> pids; echo route >> trace; exec /bin/sleep 60");
        let (stop, rx) = tokio::sync::watch::channel(false);
        let ack = Arc::new(StartupReapAck::default());
        let mut control = StartupCommandControl::new(Duration::from_secs(30), Some(rx));
        control.reap_ack = Some(ack.clone());
        let path = cli.path.clone();
        let task = tokio::spawn(async move {
            let addresses = ["192.0.2.1".parse().unwrap(), "2001:db8::1".parse().unwrap()];
            collect_routes_with(&addresses, move |address| {
                let path = path.clone();
                let control = control.clone();
                async move { route_interface_controlled(address, &path, &control).await }
            })
            .await
        });
        cli.pids(2).await;
        stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(3), ack.entered.notified())
            .await
            .unwrap();
        assert!(
            !task.is_finished(),
            "route collection cannot drop its child tasks"
        );
        ack.release.add_permits(2);
        assert!(
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .is_empty()
        );
        cli.assert_reaped(2).await;
        assert_eq!(
            cli.trace().lines().count(),
            2,
            "no retry or direct fallback after cancellation"
        );
    }

    #[test]
    fn validates_epdg_hostname_without_accepting_socket_or_url_syntax() {
        assert!(valid_hostname("epdg.epc.mnc020.mcc234.pub.3gppnetwork.org"));
        assert!(valid_hostname("epdg.example.org."));
        assert!(!valid_hostname("https://epdg.example.org"));
        assert!(!valid_hostname("epdg.example.org:500"));
        assert!(!valid_hostname("-epdg.example.org"));
    }

    #[test]
    fn parses_only_a_single_route_interface_field() {
        assert_eq!(
            parse_route_interface("route to: 87.194.8.8\ninterface: utun8\n"),
            Some("utun8".to_owned())
        );
        assert_eq!(parse_route_interface("interface:\n"), None);
    }

    #[test]
    fn surge_fallback_owns_dns_and_unverified_proxy_never_uses_host_dns() {
        assert_eq!(
            dns_owner(Some("surge-effective-profile"), true),
            DnsOwner::Surge
        );
        assert_eq!(
            dns_owner(Some("surge-enhanced-mode"), false),
            DnsOwner::Surge
        );
        assert_eq!(dns_owner(Some("explicit"), true), DnsOwner::Unavailable);
        assert_eq!(dns_owner(Some("macos-socks"), true), DnsOwner::Unavailable);
        assert_eq!(dns_owner(None, false), DnsOwner::HostDirect);
    }

    #[tokio::test]
    async fn uncertain_system_proxy_fails_before_any_dns_or_direct_udp() {
        for code in [
            "system_proxy_discovery_timeout",
            "system_proxy_discovery_unavailable",
            "system_proxy_pac_unsupported",
            "system_proxy_http_unsupported",
        ] {
            let decision = decide_transport_after_discovery(
                "",
                "epdg.invalid",
                Instant::now() + Duration::from_secs(1),
                Some(crate::system_proxy::SystemProxyDiscovery {
                    proxy: None,
                    diagnostic: Some(code),
                }),
                crate::system_proxy::SurgeEnhancedMode::Off,
            )
            .await;
            assert_eq!(
                decision.selection,
                Err(ProxyPolicyError::Socks5UdpUnavailable)
            );
            assert_eq!(decision.proxy_error, Some(code));
            assert_eq!(decision.proxy_source, None);
        }
    }

    #[tokio::test]
    async fn unreadable_surge_vif_state_fails_before_host_dns() {
        let decision = decide_transport_after_discovery(
            "",
            "epdg.invalid",
            Instant::now() + Duration::from_secs(1),
            Some(crate::system_proxy::SystemProxyDiscovery::default()),
            crate::system_proxy::SurgeEnhancedMode::Unknown,
        )
        .await;
        assert_eq!(
            decision.selection,
            Err(ProxyPolicyError::Socks5UdpUnavailable)
        );
        assert_eq!(
            decision.proxy_error,
            Some("surge_enhanced_mode_unavailable")
        );
    }

    #[test]
    fn surge_dns_reply_is_bounded_matches_query_and_keeps_address_order() {
        let reply = br#"{"domain":"WLAN.THREE.COM.HK","v4Addresses":["203.145.92.49","203.145.92.49","124.217.186.49"],"v6Addresses":["2001:db8::1"]}"#;
        assert_eq!(
            parse_surge_dns_reply("wlan.three.com.hk", reply).unwrap(),
            vec![
                "203.145.92.49".parse::<IpAddr>().unwrap(),
                "124.217.186.49".parse().unwrap(),
                "2001:db8::1".parse().unwrap(),
            ]
        );
        assert!(parse_surge_dns_reply("wlan.three.com.hk.", reply).is_ok());
        assert!(parse_surge_dns_reply("other.example", reply).is_err());
        assert!(
            parse_surge_dns_reply(
                "wlan.three.com.hk",
                br#"{"domain":"wlan.three.com.hk","v4Addresses":[]}"#
            )
            .is_err()
        );
        assert!(
            parse_surge_dns_reply("wlan.three.com.hk", &vec![b'x'; SURGE_DNS_OUTPUT_LIMIT + 1])
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_front_proxy_cannot_silently_select_direct_public_udp() {
        let decision = decide_from_proxy_probe(
            Some(Err("socks5_handshake_failed")),
            Some("explicit"),
            false,
            || async {
                (
                    vec!["203.145.92.49".parse().unwrap()],
                    vec![RouteEvidence {
                        address: "203.145.92.49".parse().unwrap(),
                        interface: Some("en0".to_owned()),
                    }],
                )
            },
        )
        .await;
        assert_eq!(
            decision.selection,
            Err(ProxyPolicyError::Socks5UdpUnavailable)
        );
        assert_eq!(decision.proxy_error, Some("socks5_handshake_failed"));
    }

    #[tokio::test]
    async fn surge_vif_without_system_proxy_rejects_an_unverified_direct_route() {
        let decision =
            decide_from_proxy_probe(None, Some("surge-enhanced-mode"), false, || async {
                (
                    vec!["203.145.92.49".parse().unwrap()],
                    vec![RouteEvidence {
                        address: "203.145.92.49".parse().unwrap(),
                        interface: Some("en0".to_owned()),
                    }],
                )
            })
            .await;
        assert_eq!(
            decision.selection,
            Err(ProxyPolicyError::UnverifiedPublicRoute)
        );
        assert_eq!(decision.proxy_source, Some("surge-enhanced-mode"));

        let routed = decide_from_proxy_probe(None, Some("surge-enhanced-mode"), false, || async {
            (
                vec!["203.145.92.49".parse().unwrap()],
                vec![RouteEvidence {
                    address: "203.145.92.49".parse().unwrap(),
                    interface: Some("utun4".to_owned()),
                }],
            )
        })
        .await;
        assert_eq!(
            routed.selection,
            Ok(ProxyMode::EnhancedUtun {
                address: "203.145.92.49".parse().unwrap(),
                interface: "utun4".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn route_lookups_run_concurrently_but_keep_dns_order() {
        let addresses = [
            "192.0.2.1".parse().unwrap(),
            "192.0.2.2".parse().unwrap(),
            "192.0.2.3".parse().unwrap(),
        ];
        let barrier = Arc::new(Barrier::new(addresses.len()));
        let routes = timeout(
            Duration::from_secs(1),
            collect_routes_with(&addresses, move |address| {
                let barrier = Arc::clone(&barrier);
                async move {
                    barrier.wait().await;
                    if address == "192.0.2.1".parse::<IpAddr>().unwrap() {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    Some("utun8".to_owned())
                }
            }),
        )
        .await
        .expect("route probes should not run serially");
        assert_eq!(
            routes.iter().map(|route| route.address).collect::<Vec<_>>(),
            addresses
        );
    }

    #[test]
    fn loopback_proxy_detection_does_not_depend_on_socks_handshake() {
        assert!(configured_proxy_is_loopback("127.0.0.1:7897"));
        assert!(configured_proxy_is_loopback("http://127.0.0.1:6153"));
        assert!(!configured_proxy_is_loopback("10.10.10.21:7890"));
        assert!(!configured_proxy_is_loopback("127.0.0.1"));
    }

    #[tokio::test]
    async fn verified_socks_does_not_resolve_epdg_locally() {
        let resolved = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&resolved);
        let proxy = "127.0.0.1:7897".parse().unwrap();

        let decision = decide_from_proxy_probe(
            Some(Ok(crate::proxy_probe::EndpointProbe {
                address: proxy,
                result: proletarius_vowifi::Socks5Probe {
                    handshake_latency_ms: 1,
                    udp_associate: true,
                },
            })),
            Some("explicit"),
            true,
            move || async move {
                observed.store(true, Ordering::SeqCst);
                (vec!["87.194.8.8".parse().unwrap()], Vec::new())
            },
        )
        .await;

        assert_eq!(decision.selection, Ok(ProxyMode::Socks5UdpDomain));
        assert_eq!(decision.proxy, Some(proxy));
        assert!(!resolved.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn tcp_only_socks_fallback_retains_udp_associate_diagnostic() {
        let proxy = "127.0.0.1:6153".parse().unwrap();
        let decision = decide_from_proxy_probe(
            Some(Ok(crate::proxy_probe::EndpointProbe {
                address: proxy,
                result: proletarius_vowifi::Socks5Probe {
                    handshake_latency_ms: 1,
                    udp_associate: false,
                },
            })),
            Some("surge-effective-profile"),
            true,
            || async {
                (
                    vec!["124.217.186.49".parse().unwrap()],
                    vec![RouteEvidence {
                        address: "124.217.186.49".parse().unwrap(),
                        interface: Some("utun8".to_owned()),
                    }],
                )
            },
        )
        .await;

        assert_eq!(
            decision.selection,
            Ok(ProxyMode::EnhancedUtun {
                address: "124.217.186.49".parse().unwrap(),
                interface: "utun8".to_owned(),
            })
        );
        assert_eq!(
            decision.proxy_error,
            Some("socks5_udp_associate_unavailable")
        );
        assert_eq!(decision.proxy_source, Some("surge-effective-profile"));
    }

    #[test]
    fn automatic_http_or_pac_diagnostic_is_preserved_without_inventing_socks() {
        let decision = attach_discovery_diagnostic(
            TransportDecision {
                selection: Ok(ProxyMode::Direct("87.194.8.8".parse().unwrap())),
                proxy: None,
                proxy_error: None,
                proxy_source: None,
            },
            Some("system_proxy_pac_unsupported"),
        );
        assert_eq!(decision.proxy_error, Some("system_proxy_pac_unsupported"));
        assert_eq!(decision.proxy_source, None);
        assert_eq!(
            decision.selection,
            Ok(ProxyMode::Direct("87.194.8.8".parse().unwrap()))
        );
    }

    #[test]
    fn only_automatic_discovery_spends_remaining_probe_budget() {
        let deadline = Instant::now() + Duration::from_millis(250);
        assert_eq!(
            probe_budget_for_selection("127.0.0.1:7897", deadline),
            Duration::from_secs(6),
            "an explicit SOCKS endpoint must retain its full UDP verification budget"
        );
        let automatic = probe_budget_for_selection("", deadline);
        assert!(
            automatic <= Duration::from_millis(250),
            "automatic probe must fit the same bounded startup selection"
        );
    }

    #[tokio::test]
    async fn direct_response_retains_nonfatal_proxy_failure() {
        let response = selection_response(
            Ok(ProxyMode::Direct("87.194.8.8".parse().unwrap())),
            None,
            Some("socks5_handshake_failed"),
            None,
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["mode"], "direct");
        assert_eq!(value["proxy_error"], "socks5_handshake_failed");
    }

    #[tokio::test]
    async fn socks_response_exposes_only_the_verified_proxy_endpoint() {
        let endpoint: SocketAddr = "127.0.0.1:7897".parse().unwrap();
        let socks = selection_response(
            Ok(ProxyMode::Socks5UdpDomain),
            Some(endpoint),
            None,
            Some("macos-socks"),
        );
        let body = socks.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["proxy_endpoint"], "127.0.0.1:7897");
        assert_eq!(value["proxy_source"], "macos-socks");

        let fake = selection_response(
            Ok(ProxyMode::EnhancedFakeIp("198.18.0.14".parse().unwrap())),
            Some(endpoint),
            None,
            Some("macos-socks"),
        );
        let body = fake.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert!(value.get("proxy_endpoint").is_none());
    }

    #[tokio::test]
    async fn verified_socks5_uses_domain_when_no_vif_route_exists() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = listener.local_addr().unwrap();
        let relay = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay_port = relay.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for connection in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut greeting = [0_u8; 3];
                stream.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, proletarius_vowifi::NO_AUTH_GREETING);
                stream.write_all(&[5, 0]).await.unwrap();
                if connection == 1 {
                    let mut associate = [0_u8; 10];
                    stream.read_exact(&mut associate).await.unwrap();
                    assert_eq!(associate, proletarius_vowifi::UDP_ASSOCIATE_REQUEST);
                    let mut answer = vec![5, 0, 0, 1, 127, 0, 0, 1];
                    answer.extend_from_slice(&relay_port.to_be_bytes());
                    stream.write_all(&answer).await.unwrap();
                    let mut closed = [0_u8; 1];
                    let _ = stream.read(&mut closed).await;
                }
            }
        });

        // localhost has no VIF route, so a successful UDP ASSOCIATE must win
        // over the otherwise direct loopback address. Do not use .invalid
        // here: an active Surge Enhanced Mode intentionally maps it to a
        // 198.18/15 fake IP and would make this test machine-dependent.
        let selected = select_for_session(&proxy.to_string(), "localhost")
            .await
            .unwrap();
        assert_eq!(selected.mode, ProxyMode::Socks5UdpDomain);
        assert_eq!(selected.proxy, Some(proxy));
        assert_eq!(selected.proxy_error, None);
        assert_eq!(selected.proxy_source, Some("explicit"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn real_local_proxy_selects_a_verified_non_direct_transport_when_enabled() {
        if std::env::var("PROLETARIUS_TEST_REAL_PROXY").as_deref() != Ok("1") {
            return;
        }
        let proxy = std::env::var("PROLETARIUS_TEST_PROXY_ENDPOINT")
            .unwrap_or_else(|_| "127.0.0.1:7897".to_owned());
        // An explicit front proxy must prove SOCKS5 UDP relay. It may not
        // silently switch to host DNS or direct public UDP if that fails.
        let response = select_transport(&proxy, "epdg.invalid").await;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert!(matches!(
            value["mode"].as_str(),
            Some("enhanced-mode-fake-ip" | "enhanced-mode-utun" | "socks5-udp-domain")
        ));
        assert!(value.get("proxy_error").is_none());
    }

    #[tokio::test]
    async fn real_system_proxy_discovery_feeds_non_direct_transport_when_enabled() {
        if std::env::var("PROLETARIUS_TEST_REAL_SURGE").as_deref() != Ok("1") {
            return;
        }
        crate::system_proxy::discover()
            .await
            .proxy
            .expect("running Surge System Proxy must be discovered");
        let selected = select_for_session("", "wlan.three.com.hk").await.unwrap();
        assert!(matches!(
            selected.mode,
            ProxyMode::EnhancedFakeIp(_)
                | ProxyMode::EnhancedUtun { .. }
                | ProxyMode::Socks5UdpDomain
        ));
        assert_eq!(selected.proxy_source, Some("surge-effective-profile"));
    }
}
