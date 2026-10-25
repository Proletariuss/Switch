//! Candidate planning and progression for the existing native REGISTER owner.
//! This does not create another transport or protocol actor.
use super::*;
use proletarius_ims::{SipError, registration_io::RegisterWaitError};
use proletarius_vowifi::{
    RequestedIpStack,
    registration_coordinator::{RegistrationError, RegistrationStage},
};

const CANDIDATE_TIMEOUT: Duration = Duration::from_secs(15);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Candidate {
    pub(super) local_ip: IpAddr,
    pub(super) registrar: SocketAddr,
    pub(super) transport: SocketAddr,
}
pub(super) fn select(
    profile: &NativeImsRegisterProfile,
    tunnel: &TunnelConfiguration,
    stack: RequestedIpStack,
) -> Result<Vec<Candidate>, NativeImsEndpointSelectionError> {
    use NativeImsEndpointSelectionError as Error;
    let (local, addresses) = if let Some(configured) = profile.configured_pcscf() {
        if (stack == RequestedIpStack::Ipv4 && configured.is_ipv6())
            || (stack == RequestedIpStack::Ipv6 && configured.is_ipv4())
        {
            return Err(Error::NoMatchingTunnelAddress);
        }
        let local = tunnel_local_address(tunnel, configured.is_ipv4())
            .ok_or(Error::NoMatchingTunnelAddress)?;
        // A numeric carrier override need not be advertised in CFG_REPLY.
        // It still uses the authenticated tunnel's matching local address.
        (local, vec![configured.to_string()])
    } else {
        let compatible = |ipv4| {
            tunnel_local_address(tunnel, ipv4).filter(|_| {
                tunnel
                    .pcscf_servers
                    .iter()
                    .any(|peer| peer.is_ipv4() == ipv4)
            })
        };
        let local = match stack {
            RequestedIpStack::Ipv4 => compatible(true),
            RequestedIpStack::Ipv6 => compatible(false),
            RequestedIpStack::Dual => compatible(false).or_else(|| compatible(true)),
        }
        .ok_or(Error::MissingTunnelPcscf)?;
        let addresses = tunnel
            .pcscf_servers
            .iter()
            .filter(|peer| peer.is_ipv4() == local.is_ipv4())
            .map(|peer| SocketAddr::new(*peer, 5060).to_string())
            .collect();
        (local, addresses)
    };
    let expanded = proletarius_ims::registrar_candidates::expand_register_candidates(
        &addresses,
        Some(&local.to_string()),
    )
    .map_err(|_| Error::InvalidCandidates)?;
    if expanded.is_empty() || expanded.len() > 128 {
        return Err(Error::InvalidCandidates);
    }
    expanded
        .into_iter()
        .map(|candidate| {
            let registrar: SocketAddr = candidate
                .registrar
                .parse()
                .map_err(|_| Error::InvalidCandidates)?;
            let transport: SocketAddr = candidate
                .transport
                .parse()
                .map_err(|_| Error::InvalidCandidates)?;
            if registrar.is_ipv4() != local.is_ipv4()
                || transport.is_ipv4() != local.is_ipv4()
                || registrar.port() == 0
                || transport.port() == 0
            {
                return Err(Error::InvalidCandidates);
            }
            Ok(Candidate {
                local_ip: local,
                registrar,
                transport,
            })
        })
        .collect()
}

fn retry_input<E>(error: &RegistrationError<E>) -> Option<(u16, &'static str)> {
    use RegistrationError as Error;
    match error {
        Error::Sip {
            stage: RegistrationStage::InitialRegister,
            source: SipError::RegistrationRejected(status),
        }
        | Error::Transport {
            stage: RegistrationStage::InitialRegister,
            source: RegisterWaitError::Sip(SipError::RegistrationRejected(status)),
        } => Some((*status, "")),
        // Authenticated/secured rejection and AKA errors are not initial-node
        // rejections. Only known transport failures may advance after auth.
        Error::Deadline(_) => Some((0, "deadline")),
        Error::Io { source, .. }
        | Error::Transport {
            source: RegisterWaitError::Io(source),
            ..
        } if source.kind() == std::io::ErrorKind::UnexpectedEof => Some((0, "partial_sip")),
        Error::Io { source, .. } if source.kind() == std::io::ErrorKind::TimedOut => {
            Some((0, "timeout_text"))
        }
        Error::Transport {
            source: RegisterWaitError::Disconnected,
            ..
        } => Some((0, "connection_closed")),
        Error::Transport {
            source: RegisterWaitError::Io(source),
            ..
        } if source.kind() == std::io::ErrorKind::TimedOut => Some((0, "timeout_text")),
        _ => None,
    }
}
fn retry_delay(
    profile: &NativeImsRegisterProfile,
    input: Option<(u16, &'static str)>,
    clean: bool,
    has_more: bool,
) -> Option<Duration> {
    if !clean {
        return None;
    }
    let (status, failure) = input?;
    let policy = profile.template().retry_policy();
    if !proletarius_ims::register_retry::should_advance_register_candidate(
        status,
        failure,
        has_more,
        policy.forbidden_status_codes(),
        policy.temporary_status_codes(),
    )
    .ok()?
    {
        return None;
    }
    let delay = proletarius_ims::register_retry::resolve_register_retry_delay_ms(
        status,
        policy.temporary_status_codes(),
        i64::try_from(policy.temporary_retry_seconds()).ok()?,
    )
    .ok()?;
    Some(Duration::from_millis(delay))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn register<A, AFut>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device: &NativeImsDeviceIdentity,
    subscriber: &NativeImsSubscriberIdentity,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    eap: Option<&proletarius_vowifi::EapDirectAuthMaterial>,
    candidates: &[Candidate],
    deadline: tokio::time::Instant,
    stop: tokio::sync::watch::Receiver<bool>,
    authenticate: A,
) -> Result<RegisteredImsSession, NativeImsRegistrationError>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, NativeAkaError>> + Send,
{
    register_with_session_mode(
        services,
        profile,
        device,
        subscriber,
        runtime,
        endpoints,
        eap,
        candidates,
        NativeImsSessionMode::VoiceAndSms,
        deadline,
        stop,
        authenticate,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn register_with_session_mode<A, AFut>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device: &NativeImsDeviceIdentity,
    subscriber: &NativeImsSubscriberIdentity,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    eap: Option<&proletarius_vowifi::EapDirectAuthMaterial>,
    candidates: &[Candidate],
    session_mode: NativeImsSessionMode,
    deadline: tokio::time::Instant,
    stop: tokio::sync::watch::Receiver<bool>,
    authenticate: A,
) -> Result<RegisteredImsSession, NativeImsRegistrationError>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, NativeAkaError>> + Send,
{
    // Existing Go opt-in only; freeze it for this registration chain.
    let environment_enabled = std::env::var("PROLETARIUS_IMS_INITIAL_REJECT_FALLBACK")
        .is_ok_and(|value| value.trim() == "1");
    register_with_environment_and_session_mode(
        services,
        profile,
        device,
        subscriber,
        runtime,
        endpoints,
        eap,
        candidates,
        deadline,
        stop,
        authenticate,
        environment_enabled,
        session_mode,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn register_with_environment<A, AFut>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device: &NativeImsDeviceIdentity,
    subscriber: &NativeImsSubscriberIdentity,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    eap: Option<&proletarius_vowifi::EapDirectAuthMaterial>,
    candidates: &[Candidate],
    deadline: tokio::time::Instant,
    stop: tokio::sync::watch::Receiver<bool>,
    authenticate: A,
    environment_enabled: bool,
) -> Result<RegisteredImsSession, NativeImsRegistrationError>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, NativeAkaError>> + Send,
{
    register_with_environment_and_session_mode(
        services,
        profile,
        device,
        subscriber,
        runtime,
        endpoints,
        eap,
        candidates,
        deadline,
        stop,
        authenticate,
        environment_enabled,
        NativeImsSessionMode::VoiceAndSms,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn register_with_environment_and_session_mode<A, AFut>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device: &NativeImsDeviceIdentity,
    subscriber: &NativeImsSubscriberIdentity,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    eap: Option<&proletarius_vowifi::EapDirectAuthMaterial>,
    candidates: &[Candidate],
    deadline: tokio::time::Instant,
    stop: tokio::sync::watch::Receiver<bool>,
    mut authenticate: A,
    environment_enabled: bool,
    session_mode: NativeImsSessionMode,
) -> Result<RegisteredImsSession, NativeImsRegistrationError>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, NativeAkaError>> + Send,
{
    for (index, candidate) in candidates.iter().enumerate() {
        let tokens = if index == 0 {
            None
        } else {
            Some(
                NativeSipTransactionIds::generate()
                    .map_err(|()| NativeImsRegistrationError::Entropy)?,
            )
        };
        let runtime = NativeImsRuntime {
            local_ip: candidate.local_ip,
            pcscf: candidate.registrar,
            call_id: tokens.as_ref().map_or(runtime.call_id, |v| &v.call_id),
            from_tag: tokens.as_ref().map_or(runtime.from_tag, |v| &v.from_tag),
            via_branch: tokens
                .as_ref()
                .map_or(runtime.via_branch, |v| &v.via_branch),
            ..runtime
        };
        let result = register_candidate(
            services,
            profile,
            device,
            subscriber,
            runtime,
            endpoints,
            eap,
            candidate.transport,
            deadline.min(tokio::time::Instant::now() + CANDIDATE_TIMEOUT),
            stop.clone(),
            &mut authenticate,
            environment_enabled,
            session_mode,
        )
        .await;
        match result {
            Ok(session) => return Ok(session),
            Err(failed) => {
                let NativeImsRegistrationError::Registration(failure) = &failed else {
                    return Err(failed);
                };
                let error = failure.registration();
                let delay = retry_delay(
                    profile,
                    retry_input(error.cause()),
                    error.cleanup().is_clean(),
                    index + 1 < candidates.len(),
                );
                if *stop.borrow()
                    || stop.has_changed().is_err()
                    || tokio::time::Instant::now() >= deadline
                    || delay.is_none()
                {
                    return Err(failed);
                }
                let wake = tokio::time::Instant::now() + delay.unwrap();
                let mut stopped = stop.clone();
                tokio::select! {
                    _ = tokio::time::sleep_until(wake.min(deadline)) => {},
                    _ = stopped.changed() => return Err(failed),
                }
                if *stop.borrow()
                    || stop.has_changed().is_err()
                    || tokio::time::Instant::now() >= deadline
                {
                    return Err(failed);
                }
            }
        }
    }
    Err(NativeImsRegistrationError::Endpoint(
        NativeImsEndpointSelectionError::InvalidCandidates,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn register_candidate<A, AFut>(
    services: RegistrationServices<'_>,
    profile: &NativeImsRegisterProfile,
    device: &NativeImsDeviceIdentity,
    subscriber: &NativeImsSubscriberIdentity,
    runtime: NativeImsRuntime<'_>,
    endpoints: NativeSecurityEndpoints,
    eap: Option<&proletarius_vowifi::EapDirectAuthMaterial>,
    transport: SocketAddr,
    deadline: tokio::time::Instant,
    stop: tokio::sync::watch::Receiver<bool>,
    authenticate: &mut A,
    environment_enabled: bool,
    session_mode: NativeImsSessionMode,
) -> Result<RegisteredImsSession, NativeImsRegistrationError>
where
    A: FnMut(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> AFut + Send,
    AFut: Future<Output = Result<SimAnswer, NativeAkaError>> + Send,
{
    let variants = profile
        .initial_register_fallback(environment_enabled, None)
        .variants;
    proletarius_vowifi::registration_coordinator::register_native_profile_with_initial_variants_and_session_mode(
        services,
        profile,
        device,
        subscriber,
        runtime,
        endpoints,
        eap,
        Some(transport),
        Some(&variants),
        environment_enabled,
        1,
        session_mode,
        deadline,
        stop,
        authenticate,
    )
    .await
    .map_err(|failure| NativeImsRegistrationError::Registration(Box::new(failure)))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod fallback_tests;
