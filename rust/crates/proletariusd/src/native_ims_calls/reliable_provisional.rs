//! Initial UAS reliable provisional responses. Their TU and PRACK server
//! transactions survive media cleanup; neither one authorizes microphone TX.
use super::*;

const RELIABLE_T1: Duration = Duration::from_millis(500);
const RELIABLE_TIMEOUT: Duration = Duration::from_secs(32);
const MAX_PRACK_REPLIES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RelSdpRole {
    AnswerToInvite,
    LocalOffer,
}

pub(super) struct ReliableProvisional {
    pub(super) response: Response,
    pub(super) rseq: u32,
    pub(super) invite_cseq: u32,
    pub(super) role: RelSdpRole,
    pub(super) confirmed: bool,
    pub(super) pracked: bool,
    pub(super) deadline: Instant,
    pub(super) next_response_retry: Option<Instant>,
    pub(super) response_retry_interval: Duration,
    pub(super) retain_until: Instant,
}
impl Drop for ReliableProvisional {
    fn drop(&mut self) {
        self.response.data.body.zeroize();
    }
}

pub(super) struct PrackReply {
    pub(super) connection: SipConnectionId,
    pub(super) request: Request,
    pub(super) response: Response,
    pub(super) confirmed: bool,
    pub(super) deadline: Instant,
}
impl PrackReply {
    fn matches(&self, connection: SipConnectionId, request: &Request) -> bool {
        let sequence_matches = match (
            cseq(&request.data.headers),
            cseq(&self.request.data.headers),
        ) {
            (Ok(current), Ok(original)) => current == original,
            _ => {
                single(&request.data.headers, "CSeq").ok()
                    == single(&self.request.data.headers, "CSeq").ok()
            }
        };
        let via_matches = same_via(&request.data.headers, &self.request.data.headers)
            || request.data.headers.get("Via").map(Header::value)
                == self.request.data.headers.get("Via").map(Header::value);
        connection == self.connection
            && request.method == Method::PRACK
            && sequence_matches
            && via_matches
    }
}
impl Drop for PrackReply {
    fn drop(&mut self) {
        self.request.data.body.zeroize();
        self.response.data.body.zeroize();
    }
}

impl Call {
    pub(super) fn reliable_next_deadline(&self) -> Option<Instant> {
        let provisional = self.reliable.as_ref().into_iter().flat_map(|pending| {
            [
                pending.next_response_retry,
                (!pending.pracked && pending.next_response_retry.is_some())
                    .then_some(pending.deadline),
                Some(pending.retain_until),
            ]
            .into_iter()
            .flatten()
        });
        provisional
            .chain(self.prack_replies.iter().map(|reply| reply.deadline))
            .min()
    }

    fn reliable_invite_unfinished(&self) -> bool {
        self.initial_reply.is_none()
            && matches!(&self.tracker, Tracker::Incoming(tracker)
                if matches!(tracker.phase(), IncomingCallPhase::Trying | IncomingCallPhase::Ringing))
    }

    pub(super) fn reliable_protected(&self, now: Instant) -> bool {
        self.reliable.as_ref().is_some_and(|pending| {
            pending.retain_until > now || (!pending.pracked && self.reliable_invite_unfinished())
        }) || self.prack_replies.iter().any(|reply| reply.deadline > now)
    }

    pub(super) fn prune_reliable(&mut self, now: Instant) {
        self.prack_replies.retain(|reply| reply.deadline > now);
        if self.reliable.as_ref().is_some_and(|pending| {
            pending.retain_until <= now && (pending.pracked || !self.reliable_invite_unfinished())
        }) {
            self.reliable = None;
        }
    }

    pub(super) fn stop_reliable_tu(&mut self) {
        if let Some(pending) = self.reliable.as_mut() {
            pending.next_response_retry = None;
            // Keep the old dialog/RAck identity after a final rejection.
            pending.retain_until = pending.retain_until.max(Instant::now() + RELIABLE_TIMEOUT);
        }
    }

    pub(super) fn clear_reliable(&mut self) {
        self.reliable = None;
        self.prack_replies.clear();
        self.answer_requested = false;
        self.early_oa_complete = false;
    }
}

fn parse_rack(headers: &Headers) -> Result<(u32, u32)> {
    let invalid = || Failure::known("ims_prack_rack_invalid");
    let value = single(headers, "RAck").map_err(|_| invalid())?;
    let mut parts = value.split_ascii_whitespace();
    let number = |value: Option<&str>, maximum: u32| {
        let value = value.ok_or_else(invalid)?;
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        value
            .parse::<u32>()
            .ok()
            .filter(|number| *number > 0 && *number <= maximum)
            .ok_or_else(invalid)
    };
    let response = number(parts.next(), u32::MAX)?;
    let invite = number(parts.next(), i32::MAX as u32)?;
    if parts.next() != Some("INVITE") || parts.next().is_some() {
        return Err(invalid());
    }
    Ok((response, invite))
}

fn valid_prack_via(headers: &Headers) -> bool {
    headers.get("Via").is_some_and(|header| {
        header.value().split(';').any(|part| {
            part.trim().strip_prefix("branch=").is_some_and(|branch| {
                branch.starts_with("z9hG4bK")
                    && branch.len() > "z9hG4bK".len()
                    && !branch
                        .bytes()
                        .any(|byte| byte.is_ascii_whitespace() || byte == b',')
            })
        })
    })
}

fn has_sdp(request: &Request) -> bool {
    single(&request.data.headers, "Content-Type")
        .ok()
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/sdp"))
}

/// A rejected stream is not an advertisement of an unavailable codec. Keep
/// every offered m-line in order, but make each port zero (RFC 3264 section 6).
fn rejected_answer(offer: &[u8], session_id: u64, version: u64, ip: IpAddr) -> Result<Vec<u8>> {
    let invalid = || Failure::known("ims_prack_sdp_invalid");
    if offer.len() > 64 * 1024 {
        return Err(invalid());
    }
    let offer = std::str::from_utf8(offer).map_err(|_| invalid())?;
    let family = if ip.is_ipv4() { "IP4" } else { "IP6" };
    let timing = offer
        .lines()
        .find(|line| line.starts_with("t="))
        .ok_or_else(invalid)?;
    let mut media = String::new();
    for line in offer.lines().filter(|line| line.starts_with("m=")) {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() < 4 || !fields[0].starts_with("m=") || fields[0].len() <= 2 {
            return Err(invalid());
        }
        media.push_str(&format!("{} 0 {} {}\r\n", fields[0], fields[2], fields[3]));
        media.push_str("a=inactive\r\n");
    }
    if !offer.lines().any(|line| line == "v=0") || media.is_empty() {
        return Err(invalid());
    }
    Ok(format!(
        "v=0\r\no=- {session_id} {version} IN {family} {ip}\r\ns=Switch\r\nc=IN {family} {ip}\r\n{timing}\r\n{media}"
    )
    .into_bytes())
}

struct PrackMedia {
    remote: AudioNegotiation,
    history: sdp::PayloadHistory,
    local_answer: Option<Vec<u8>>,
    version: u64,
}

impl Service {
    fn reliable_owner(&self, retired: Option<usize>) -> &Call {
        match retired {
            Some(index) => &self.retired[index],
            None => self.call.as_ref().expect("matched reliable call"),
        }
    }
    fn reliable_owner_mut(&mut self, retired: Option<usize>) -> &mut Call {
        match retired {
            Some(index) => &mut self.retired[index],
            None => self.call.as_mut().expect("matched reliable call"),
        }
    }

    /// End an early INVITE without a BYE. Even a failed final-response write
    /// must not bypass draining every staged/native media resource.
    pub(super) async fn end_reliable_early(
        &mut self,
        mut error: Failure,
        status: u16,
        reason: &str,
    ) -> Result<()> {
        let response = {
            let call = self
                .call
                .as_mut()
                .ok_or_else(|| Failure::known("ims_call_missing"))?;
            if call
                .initial_reply
                .as_ref()
                .is_some_and(|response| (200..300).contains(&response.status))
            {
                return Err(Failure::known("ims_reliable_not_early"));
            }
            call.stop_reliable_tu();
            call.answer_requested = false;
            if let Tracker::Incoming(tracker) = &mut call.tracker {
                tracker.end();
            }
            reply_for(
                &call.request,
                status,
                reason,
                Some(&call.local_tag),
                Vec::new(),
                None,
            )
        };
        self.revoke_media_bridge();
        let write = match response {
            Ok(response) => self.initial_response(response).await,
            Err(error) => Err(error),
        };
        if let Err(write) = &write {
            error.uncertain |= write.uncertain;
        }
        let cleanup = self.fail(&error).await;
        write?;
        cleanup
    }

    pub(super) async fn start_reliable_ringing(&mut self) -> Result<()> {
        let call = self
            .call
            .as_ref()
            .ok_or_else(|| Failure::known("ims_call_missing"))?;
        if call.reliable.is_some() {
            return Ok(());
        }
        let binding = call.binding.clone();
        let offered = !call.request.data.body.is_empty();
        let original_remote = call.remote.clone();
        if offered
            && (original_remote.is_none() || unsupported_media_direction(&call.request.data.body))
        {
            let error = Failure::known("ims_call_remote_sdp_invalid");
            self.end_reliable_early(error.clone(), 488, "Not Acceptable Here")
                .await?;
            return Err(error);
        }
        if call.prack_replies.len() >= MAX_PRACK_REPLIES {
            let error = Failure::known("ims_prack_reply_capacity");
            self.end_reliable_early(error.clone(), 503, "Service Unavailable")
                .await?;
            return Err(error);
        }
        let local = match self.media.clone().allocate(binding.clone()).await {
            Ok(local) => local,
            Err(error) => {
                self.end_reliable_early(error.clone(), 500, "Server Internal Error")
                    .await?;
                return Err(error);
            }
        };
        let call = self.call.as_mut().unwrap();
        call.media_allocated = true;
        call.local_media = Some((local.ip, local.port));
        self.snapshot.audio_port = local.audio_port;
        self.snapshot.audio_token = local.audio_token;
        let sdp = if let Some(remote) = original_remote.as_ref() {
            sdp::build_local_audio_answer_to_offer_version(
                binding.generation.value(),
                call.local_sdp_version,
                local.ip,
                local.port,
                std::str::from_utf8(&call.request.data.body)
                    .map_err(|_| Failure::known("ims_call_remote_sdp_invalid"))?,
                remote,
                self.config.evs_only,
            )
        } else {
            sdp::build_local_sdp_version(
                binding.generation.value(),
                call.local_sdp_version,
                local.ip,
                local.port,
                false,
                None,
                None,
                None,
                self.config.evs_only,
            )
        };
        let body = match sdp {
            Ok(sdp) => sdp.into_bytes(),
            Err(_) => {
                let error = Failure::known("ims_call_local_sdp_invalid");
                self.end_reliable_early(error.clone(), 488, "Not Acceptable Here")
                    .await?;
                return Err(error);
            }
        };
        let history = match self.proposed_history(&body) {
            Ok(history) => history,
            Err(error) => {
                self.end_reliable_early(error.clone(), 488, "Not Acceptable Here")
                    .await?;
                return Err(error);
            }
        };
        let preparation = if let Some(remote) = original_remote.as_ref() {
            self.media
                .clone()
                .prepare(binding.clone(), remote.clone())
                .await
        } else {
            self.media
                .clone()
                .prepare_local_offer(binding.clone(), body.clone())
                .await
        };
        if let Err(error) = preparation {
            self.end_reliable_early(error.clone(), 488, "Not Acceptable Here")
                .await?;
            return Err(error);
        }
        let mut random = [0; 4];
        if getrandom::fill(&mut random).is_err() {
            let error = Failure::known("ims_media_entropy_failed");
            self.end_reliable_early(error.clone(), 500, "Server Internal Error")
                .await?;
            return Err(error);
        }
        let rseq = u32::from_be_bytes(random) % (i32::MAX as u32) + 1;
        let call = self.call.as_mut().unwrap();
        let mut response = reply_for(
            &call.request,
            180,
            "Ringing",
            Some(&call.local_tag),
            body.clone(),
            Some(&self.config.contact),
        )?;
        push(&mut response.data.headers, "Require", "100rel")?;
        push(&mut response.data.headers, "RSeq", &rseq.to_string())?;
        let now = Instant::now();
        call.local_sdp = body;
        call.payload_history = history;
        if let Tracker::Incoming(tracker) = &mut call.tracker {
            if tracker.phase() == IncomingCallPhase::Trying {
                tracker
                    .ringing()
                    .map_err(|_| Failure::known("ims_call_policy_invalid"))?;
            }
        }
        call.reliable = Some(ReliableProvisional {
            response: response.clone(),
            rseq,
            invite_cseq: binding.invite_cseq,
            role: if offered {
                RelSdpRole::AnswerToInvite
            } else {
                RelSdpRole::LocalOffer
            },
            confirmed: false,
            pracked: false,
            deadline: now + RELIABLE_TIMEOUT,
            next_response_retry: None,
            response_retry_interval: RELIABLE_T1,
            retain_until: now + RELIABLE_TIMEOUT * 2,
        });
        let connection = call.connection;
        let write = self.respond(connection, response).await;
        let now = Instant::now();
        let pending = self.call.as_mut().unwrap().reliable.as_mut().unwrap();
        pending.confirmed = write.is_ok();
        pending.deadline = now + RELIABLE_TIMEOUT;
        pending.retain_until = pending.deadline + RELIABLE_TIMEOUT;
        if let Err(error) = write {
            self.end_reliable_early(error.clone(), 500, "Server Internal Error")
                .await?;
            return Err(error);
        }
        pending.next_response_retry = Some(now + RELIABLE_T1);
        self.call.as_mut().unwrap().deadline = Some(now + DECISION_TIMEOUT);
        if let Some(remote) = original_remote {
            let result = async {
                self.media
                    .clone()
                    .configure(binding.clone(), remote.clone())
                    .await?;
                self.media.clone().start(binding, true).await
            }
            .await;
            if let Err(error) = result {
                self.end_reliable_early(error.clone(), 500, "Server Internal Error")
                    .await?;
                return Err(error);
            }
            self.call.as_mut().unwrap().media_started = true;
            self.publish_codec(remote.codec);
            self.snapshot.media_ready = true;
            self.snapshot.early_media = true;
        }
        self.observe();
        Ok(())
    }

    async fn write_prack_reply(
        &mut self,
        owner: Option<usize>,
        connection: SipConnectionId,
        request: Request,
        response: Response,
    ) -> Result<()> {
        let call = self.reliable_owner_mut(owner);
        call.prune_reliable(Instant::now());
        // A peer cannot fill every slot with rejected PRACKs and prevent the
        // actual confirmation. On overload emit no uncached final: otherwise
        // a later mutation of that transaction could acquire a second result.
        let limit = if response.status >= 300
            && call
                .reliable
                .as_ref()
                .is_some_and(|pending| !pending.pracked)
        {
            MAX_PRACK_REPLIES - 1
        } else {
            MAX_PRACK_REPLIES
        };
        if call.prack_replies.len() >= limit {
            return Err(Failure::known("ims_prack_reply_capacity"));
        }
        call.prack_replies.push_back(PrackReply {
            connection,
            request,
            response: response.clone(),
            confirmed: false,
            deadline: Instant::now() + RELIABLE_TIMEOUT,
        });
        let write = self.respond(connection, response).await;
        let reply = self
            .reliable_owner_mut(owner)
            .prack_replies
            .back_mut()
            .unwrap();
        reply.confirmed = write.is_ok();
        reply.deadline = Instant::now() + RELIABLE_TIMEOUT;
        write
    }

    async fn prack_error_reply(
        &mut self,
        owner: Option<Option<usize>>,
        connection: SipConnectionId,
        request: Request,
        status: u16,
        reason: &str,
    ) -> Result<()> {
        let response = reply_for(&request, status, reason, None, Vec::new(), None)?;
        if let Some(owner) = owner {
            self.write_prack_reply(owner, connection, request, response)
                .await
        } else {
            self.respond(connection, response).await
        }
    }

    pub(super) async fn handle_reliable_prack(
        &mut self,
        connection: SipConnectionId,
        request: Request,
    ) -> Result<()> {
        let owners = std::iter::once(None)
            .filter(|_| self.call.is_some())
            .chain((0..self.retired.len()).map(Some))
            .collect::<Vec<_>>();
        let known_dialog = owners.iter().copied().find(|owner| {
            let call = self.reliable_owner(*owner);
            connection == call.connection && same_dialog(call, &request)
        });
        // A server transaction owns its original result even if a duplicate
        // changes its body/RAck, advances no longer-current CSeq, or arrives
        // after a newer call has replaced this dialog.
        for owner in &owners {
            let call = self.reliable_owner(*owner);
            if same_dialog(call, &request) {
                if let Some(reply) = call.prack_replies.iter().find(|reply| {
                    reply.deadline > Instant::now() && reply.matches(connection, &request)
                }) {
                    return if reply.confirmed {
                        self.respond(connection, reply.response.clone()).await
                    } else {
                        Ok(())
                    };
                }
            }
        }
        let (sequence, method) = match cseq(&request.data.headers) {
            Ok(value) if value.1 == Method::PRACK => value,
            _ => {
                return self
                    .prack_error_reply(known_dialog, connection, request, 400, "Bad Request")
                    .await;
            }
        };
        let rack = match parse_rack(&request.data.headers) {
            Ok(rack) if valid_prack_via(&request.data.headers) => rack,
            _ => {
                return self
                    .prack_error_reply(known_dialog, connection, request, 400, "Bad Request")
                    .await;
            }
        };
        let now = Instant::now();
        let owner = owners.into_iter().find(|owner| {
            let call = self.reliable_owner(*owner);
            connection == call.connection
                && same_dialog(call, &request)
                && call.reliable.as_ref().is_some_and(|pending| {
                    !pending.pracked
                        && pending.retain_until > now
                        && (pending.rseq, pending.invite_cseq) == rack
                })
        });
        let Some(owner) = owner else {
            return self
                .prack_error_reply(
                    known_dialog,
                    connection,
                    request,
                    481,
                    "Call/Transaction Does Not Exist",
                )
                .await;
        };
        let call = self.reliable_owner_mut(owner);
        call.prune_reliable(now);
        if call.prack_replies.len() >= MAX_PRACK_REPLIES {
            // Check before either the remote CSeq or reliability state moves.
            return Err(Failure::known("ims_prack_reply_capacity"));
        }
        if !call
            .dialog
            .as_mut()
            .is_some_and(|dialog| dialog.accept_remote_cseq(sequence, &method).is_ok())
        {
            return self
                .prack_error_reply(
                    Some(owner),
                    connection,
                    request,
                    500,
                    "Server Internal Error",
                )
                .await;
        }
        let pending = call.reliable.as_mut().unwrap();
        pending.pracked = true;
        pending.next_response_retry = None;
        pending.retain_until = pending.retain_until.max(now + RELIABLE_TIMEOUT);
        let role = pending.role;
        let local_offer = pending.response.data.body.clone();
        let expired = pending.deadline <= now;
        let live = owner.is_none()
            && !terminal(&self.snapshot.state)
            && !self.call.as_ref().unwrap().uncertain
            && self.call.as_ref().unwrap().initial_reply.is_none();
        let mut termination = (live && expired).then_some((
            Failure::known("ims_reliable_prack_timeout"),
            504,
            "Server Time-out",
        ));
        let live = live && !expired;
        let mut body = Vec::new();
        let mut media = None;
        if live {
            let outcome = match role {
                RelSdpRole::LocalOffer => {
                    if request.data.body.is_empty()
                        || !has_sdp(&request)
                        || unsupported_media_direction(&request.data.body)
                    {
                        Err(Failure::known("ims_prack_sdp_invalid"))
                    } else {
                        self.negotiate_answer(&request.data.body, &local_offer)
                            .and_then(|remote| {
                                self.proposed_history(&request.data.body).map(|history| {
                                    PrackMedia {
                                        remote,
                                        history,
                                        local_answer: None,
                                        version: self.call.as_ref().unwrap().local_sdp_version,
                                    }
                                })
                            })
                            .map(Some)
                    }
                }
                RelSdpRole::AnswerToInvite if request.data.body.is_empty() => Ok(None),
                RelSdpRole::AnswerToInvite => {
                    if !has_sdp(&request) || unsupported_media_direction(&request.data.body) {
                        Err(Failure::known("ims_prack_sdp_invalid"))
                    } else {
                        self.prack_new_offer(&request).await.map(Some)
                    }
                }
            };
            match outcome {
                Ok(candidate) => {
                    if let Some(candidate) = &candidate {
                        body = candidate.local_answer.clone().unwrap_or_default();
                    }
                    media = candidate;
                }
                Err(error) => {
                    if role == RelSdpRole::AnswerToInvite {
                        let call = self.call.as_ref().unwrap();
                        body = rejected_answer(
                            &request.data.body,
                            self.generation.value(),
                            call.local_sdp_version.saturating_add(1),
                            self.config.local_ip,
                        )
                        .unwrap_or_default();
                    }
                    let internal = error.code.starts_with("ims_media_")
                        || error.code == "ims_call_local_sdp_invalid";
                    termination = Some(if internal {
                        (error, 500, "Server Internal Error")
                    } else {
                        (error, 488, "Not Acceptable Here")
                    });
                }
            }
        } else if role == RelSdpRole::AnswerToInvite && !request.data.body.is_empty() {
            // A late PRACK may acknowledge an old answer with a new offer.
            // Reject its streams honestly, without acquiring current media.
            let call = self.reliable_owner(owner);
            body = rejected_answer(
                &request.data.body,
                call.binding.generation.value(),
                call.local_sdp_version.saturating_add(1),
                self.config.local_ip,
            )
            .unwrap_or_default();
        }
        let response = reply_for(&request, 200, "OK", None, body, None)?;
        // A failed live outcome already decided termination. The PRACK200
        // receipt may block; revoke before that wait, including an expired
        // current provisional whose local `live` flag was cleared above.
        // Retired dialog transactions never revoke the current call's bridge.
        if owner.is_none() && termination.is_some() {
            self.revoke_media_bridge();
        }
        if let Err(error) = self
            .write_prack_reply(owner, connection, request, response)
            .await
        {
            if live || termination.is_some() {
                self.end_reliable_early(error.clone(), 500, "Server Internal Error")
                    .await?;
            } else {
                self.reliable_owner_mut(owner).uncertain |= error.uncertain;
            }
            return Err(error);
        }
        if let Some((error, status, reason)) = termination {
            self.end_reliable_early(error.clone(), status, reason)
                .await?;
            return if status == 504 { Ok(()) } else { Err(error) };
        }
        if !live {
            return Ok(());
        }
        if let Some(candidate) = media {
            let binding = self.call.as_ref().unwrap().binding.clone();
            if let Err(error) = self
                .media
                .clone()
                .configure(binding.clone(), candidate.remote.clone())
                .await
            {
                self.end_reliable_early(error.clone(), 500, "Server Internal Error")
                    .await?;
                return Err(error);
            }
            if !self.call.as_ref().unwrap().media_started {
                if let Err(error) = self.media.clone().start(binding, true).await {
                    self.end_reliable_early(error.clone(), 500, "Server Internal Error")
                        .await?;
                    return Err(error);
                }
                self.call.as_mut().unwrap().media_started = true;
            }
            self.publish_codec(candidate.remote.codec);
            let call = self.call.as_mut().unwrap();
            call.remote = Some(candidate.remote);
            call.payload_history = candidate.history;
            if let Some(answer) = candidate.local_answer {
                call.local_sdp.zeroize();
                call.local_sdp = answer;
                call.local_sdp_version = candidate.version;
            }
        }
        self.call.as_mut().unwrap().early_oa_complete = true;
        self.snapshot.media_ready = true;
        self.snapshot.early_media = true;
        self.observe();
        if self.call.as_ref().unwrap().answer_requested {
            self.answer(self.generation).await?;
        }
        Ok(())
    }

    async fn prack_new_offer(&mut self, request: &Request) -> Result<PrackMedia> {
        let remote = self.negotiate_offer(&request.data.body)?;
        let history = self.proposed_history(&request.data.body)?;
        let call = self.call.as_ref().unwrap();
        let (ip, port) = call
            .local_media
            .ok_or_else(|| Failure::known("ims_media_unconfigured"))?;
        let version = call
            .local_sdp_version
            .checked_add(1)
            .ok_or_else(|| Failure::known("ims_call_local_sdp_invalid"))?;
        let answer = sdp::build_local_audio_answer_to_offer_version(
            self.generation.value(),
            version,
            ip,
            port,
            std::str::from_utf8(&request.data.body)
                .map_err(|_| Failure::known("ims_prack_sdp_invalid"))?,
            &remote,
            self.config.evs_only,
        )
        .map_err(|_| Failure::known("ims_call_local_sdp_invalid"))?
        .into_bytes();
        self.media
            .clone()
            .prepare(call.binding.clone(), remote.clone())
            .await?;
        Ok(PrackMedia {
            remote,
            history,
            local_answer: Some(answer),
            version,
        })
    }

    pub(super) async fn tick_reliable_provisionals(&mut self, now: Instant) -> Result<()> {
        if self.call.as_ref().is_some_and(|call| {
            call.reliable_invite_unfinished()
                && call
                    .reliable
                    .as_ref()
                    .is_some_and(|pending| !pending.pracked && pending.deadline <= now)
        }) {
            self.end_reliable_early(
                Failure::known("ims_reliable_prack_timeout"),
                504,
                "Server Time-out",
            )
            .await?;
        }
        let retry = self.call.as_ref().and_then(|call| {
            call.reliable
                .as_ref()
                .filter(|pending| {
                    call.reliable_invite_unfinished()
                        && pending.confirmed
                        && !pending.pracked
                        && pending
                            .next_response_retry
                            .is_some_and(|retry| retry <= now)
                        && now < pending.deadline
                })
                .map(|pending| (call.connection, pending.response.clone()))
        });
        if let Some((connection, response)) = retry {
            if let Err(error) = self.respond(connection, response).await {
                self.end_reliable_early(error.clone(), 500, "Server Internal Error")
                    .await?;
                return Err(error);
            }
            let pending = self.call.as_mut().unwrap().reliable.as_mut().unwrap();
            // RFC 3262 deliberately has no T2 cap for reliable 1xx.
            pending.response_retry_interval *= 2;
            pending.next_response_retry = Some(now + pending.response_retry_interval);
        }
        for call in self.call.iter_mut().chain(self.retired.iter_mut()) {
            call.prune_reliable(now);
        }
        Ok(())
    }
}
