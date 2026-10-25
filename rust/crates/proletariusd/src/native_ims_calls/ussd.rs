//! Signaling-only USSI on the same registered actor, never voice media.
use super::*;
use proletarius_ims::{
    ussd_dialog::{self, CommandAction, InboundRequestAction, InviteResponseAction},
    ussd_xml,
};
const MIME: &str = "application/vnd.3gpp.ussd+xml";
#[derive(Clone, Serialize)]
pub(crate) struct UssdResult {
    pub session_id: String,
    pub text: String,
    pub language: String,
    pub needs_input: bool,
    pub complete: bool,
}
pub(crate) struct Pending {
    pub session_id: String,
    pub receiver: oneshot::Receiver<Result<UssdResult>>,
}
pub(super) struct Session {
    request: Request,
    dialog: Option<Dialog>,
    pending: std::collections::VecDeque<(SipConnectionId, Request)>,
    reply: Option<oneshot::Sender<Result<UssdResult>>>,
    deadline: Instant,
    waiting_input: bool,
    cancelling: bool,
    bye_sent: bool,
    finished: bool,
    unknown: bool,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.request.data.body.zeroize();
        for (_, request) in &mut self.pending {
            request.data.body.zeroize();
        }
    }
}
impl Session {
    fn id(&self) -> &str {
        self.request.data.headers.get("Call-ID").unwrap().value()
    }
    fn deliver(&mut self, result: Result<UssdResult>) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(result);
        }
    }
}
impl Service {
    pub(crate) async fn start_ussd(
        &mut self,
        generation: SessionGeneration,
        command: &str,
    ) -> Result<Pending> {
        self.check(generation)?;
        if self.config.sms_only {
            return Err(Failure::known("ims_ussd_disabled_for_sms_session"));
        }
        if self.ussd.as_ref().is_some_and(|u| !u.finished || u.unknown) {
            return Err(Failure::known("ims_ussd_busy"));
        }
        if !command.ends_with('#')
            || !command
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'*' | b'#' | b'+'))
        {
            return Err(Failure::known("ims_ussd_invalid_command"));
        }
        let domain = self.config.home_domain.trim();
        if domain.is_empty() || domain.contains(['\r', '\n', ';', '@', '<', '>']) {
            return Err(Failure::known("ims_ussd_domain_invalid"));
        }
        let target = format!(
            "sip:{};phone-context={domain}@{domain};user=dialstring",
            command.replace('#', "%23")
        );
        let (body, content_type) =
            ussd_xml::build_invite_body(command, &self.config.local_ip.to_string())
                .map_err(|_| Failure::known("ims_ussd_input_invalid"))?;
        let id = unique();
        let tag = unique();
        let mut request = self.initial(&target, &id, 1, &tag, body)?;
        // Preserve USSI's signaling-only service selector and no RTP allocation.
        remove(
            &mut request.data.headers,
            &[
                "P-Preferred-Service",
                "Accept-Contact",
                "Session-Expires",
                "Min-SE",
                "Accept",
                "Content-Type",
                "Allow",
                "P-Early-Media",
                "Route",
                "P-Access-Network-Info",
            ],
        );
        for route in &self.config.service_routes {
            push(&mut request.data.headers, "Route", route)?;
        }
        if self.config.ussd_service_selector {
            push(
                &mut request.data.headers,
                "Accept-Contact",
                "*;+g.3gpp.ussd",
            )?;
            push(
                &mut request.data.headers,
                "P-Preferred-Service",
                "urn:urn-7:3gpp-service.ims.icsi.ussd",
            )?;
        } else {
            push(
                &mut request.data.headers,
                "Accept-Contact",
                "*;+g.3gpp.icsi-ref=\"urn%3Aurn-7%3A3gpp-service.ims.icsi.mmtel\"",
            )?;
        }
        if !self.config.security_verify.is_empty() {
            push(&mut request.data.headers, "Require", "sec-agree")?;
            push(&mut request.data.headers, "Proxy-Require", "sec-agree")?;
        }
        push(&mut request.data.headers, "Recv-Info", "g.3gpp.ussd")?;
        push(
            &mut request.data.headers,
            "Accept",
            &format!("application/sdp, multipart/mixed, {MIME}"),
        )?;
        push(
            &mut request.data.headers,
            "Allow",
            "INVITE,ACK,CANCEL,BYE,INFO",
        )?;
        push(&mut request.data.headers, "Content-Type", &content_type)?;
        let (reply, receiver) = oneshot::channel();
        if let Some(old) = self.ussd.take() {
            self.retired_ussd.push_back(old);
            while self.retired_ussd.len() > 8 {
                self.retired_ussd.pop_front();
            }
        }
        self.ussd = Some(Session {
            request: request.clone(),
            dialog: None,
            pending: Default::default(),
            reply: Some(reply),
            deadline: Instant::now() + Duration::from_secs(120),
            waiting_input: false,
            cancelling: false,
            bye_sent: false,
            finished: false,
            unknown: false,
        });
        if let Err(error) = self.send(request).await {
            let u = self.ussd.as_mut().unwrap();
            u.unknown = error.uncertain;
            u.finished = true;
            u.deliver(Err(error.clone()));
            return Err(error);
        }
        Ok(Pending {
            session_id: id,
            receiver,
        })
    }
    pub(crate) async fn continue_ussd(
        &mut self,
        generation: SessionGeneration,
        id: &str,
        input: &str,
    ) -> Result<Pending> {
        self.check(generation)?;
        let xml = ussd_xml::encode(input).map_err(|_| Failure::known("ims_ussd_input_invalid"))?;
        let u = self
            .ussd
            .as_mut()
            .filter(|u| u.id() == id && !u.finished && !u.unknown)
            .ok_or_else(|| Failure::known("ims_ussd_stale_session"))?;
        if ussd_dialog::command_action(u.dialog.is_some(), u.waiting_input, false)
            != CommandAction::SendInput
        {
            return Err(Failure::known("ims_ussd_waiting_network"));
        }
        let via = self.via();
        let u = self.ussd.as_mut().unwrap();
        let mut request = u
            .dialog
            .as_mut()
            .unwrap()
            .build_request(Method::INFO, via, xml)
            .map_err(|_| Failure::known("ims_ussd_dialog_invalid"))?;
        push(&mut request.data.headers, "Info-Package", "g.3gpp.ussd")?;
        push(
            &mut request.data.headers,
            "Content-Disposition",
            "Info-Package",
        )?;
        push(&mut request.data.headers, "Content-Type", MIME)?;
        self.decorate(&mut request)?;
        let (reply, receiver) = oneshot::channel();
        let u = self.ussd.as_mut().unwrap();
        u.reply = Some(reply);
        u.waiting_input = false;
        if let Err(error) = self.send(request).await {
            let u = self.ussd.as_mut().unwrap();
            u.unknown = error.uncertain;
            u.deliver(Err(error.clone()));
            return Err(error);
        }
        Ok(Pending {
            session_id: id.into(),
            receiver,
        })
    }
    pub(crate) async fn cancel_ussd(
        &mut self,
        generation: SessionGeneration,
        id: &str,
    ) -> Result<UssdResult> {
        self.check(generation)?;
        let u = self
            .ussd
            .as_ref()
            .filter(|u| u.id() == id && !u.finished && !u.unknown)
            .ok_or_else(|| Failure::known("ims_ussd_stale_session"))?;
        if ussd_dialog::command_action(u.dialog.is_some(), u.waiting_input, true)
            != CommandAction::Cancel
        {
            return Err(Failure::known("ims_ussd_waiting_network"));
        }
        self.terminate_ussd(false).await?;
        Ok(UssdResult {
            session_id: id.into(),
            text: String::new(),
            language: String::new(),
            needs_input: false,
            complete: true,
        })
    }
    pub(super) fn ussd_active(&self) -> bool {
        self.ussd.as_ref().is_some_and(|u| !u.finished || u.unknown)
    }
    pub(super) fn ussd_deadline(&self) -> Option<Instant> {
        self.ussd
            .as_ref()
            .filter(|u| !u.finished)
            .map(|u| u.deadline)
    }
    pub(super) async fn tick_ussd(&mut self, now: Instant) -> Result<()> {
        if self
            .ussd
            .as_ref()
            .is_some_and(|u| !u.finished && u.deadline <= now)
        {
            self.terminate_ussd(true).await?;
        }
        Ok(())
    }
    pub(super) fn shutdown_ussd(&mut self) {
        if let Some(u) = &mut self.ussd {
            u.finished = true;
            u.deliver(Err(Failure::known("ims_ussd_session_stopped")));
        }
    }
    async fn terminate_ussd(&mut self, timeout: bool) -> Result<()> {
        let via = self.via();
        let u = self.ussd.as_mut().unwrap();
        let mut request = if let Some(dialog) = &mut u.dialog {
            u.bye_sent = true;
            dialog
                .build_request(Method::BYE, via, vec![])
                .map_err(|_| Failure::known("ims_ussd_dialog_invalid"))?
        } else {
            u.cancelling = true;
            transaction(&u.request, Method::CANCEL, None)?
        };
        self.decorate(&mut request)?;
        let result = self.send(request).await;
        let u = self.ussd.as_mut().unwrap();
        u.finished = true;
        u.unknown = result.as_ref().is_err_and(|e| e.uncertain);
        u.deliver(Err(Failure::known(if timeout {
            "ims_ussd_timeout"
        } else {
            "ims_ussd_cancelled"
        })));
        result
    }
    pub(super) async fn handle_ussd(&mut self, event: &Event) -> Result<bool> {
        let RegisteredSipEvent::Message {
            connection,
            message,
            ..
        } = &event.event
        else {
            return Ok(false);
        };
        let headers = match message.as_ref() {
            RegisteredSipMessage::Request { headers, .. }
            | RegisteredSipMessage::Response { headers, .. } => headers,
        };
        if self
            .ussd
            .as_ref()
            .is_none_or(|u| single(headers, "Call-ID").ok() != Some(u.id()))
        {
            if let Some(index) = self
                .retired_ussd
                .iter()
                .position(|u| single(headers, "Call-ID").ok() == Some(u.id()))
            {
                let old = self.retired_ussd.remove(index).unwrap();
                let current = self.ussd.replace(old);
                let result = Box::pin(self.handle_ussd(event)).await;
                let old = self.ussd.take().unwrap();
                self.ussd = current;
                self.retired_ussd.insert(index, old);
                return result;
            }
            return Ok(false);
        }
        match message.as_ref() {
            RegisteredSipMessage::Request {
                method,
                uri,
                version,
                headers,
                body,
            } => {
                let request = Request {
                    method: method.clone(),
                    uri: uri.clone(),
                    data: MessageData::new(version.clone(), headers.clone(), body.to_vec()),
                };
                self.ussd_request(*connection, request).await?;
            }
            RegisteredSipMessage::Response {
                status,
                reason,
                version,
                headers,
                body,
            } => {
                if *connection != SipConnectionId::Primary {
                    return Ok(true);
                }
                let response = Response {
                    status: *status,
                    reason: reason.clone(),
                    data: MessageData::new(version.clone(), headers.clone(), body.to_vec()),
                };
                self.ussd_response(response).await?;
            }
        }
        Ok(true)
    }
    async fn ussd_response(&mut self, mut response: Response) -> Result<()> {
        let (seq, method) = cseq(&response.data.headers)?;
        let u = self.ussd.as_ref().unwrap();
        if method == Method::INFO {
            if response.status >= 300 {
                self.ussd
                    .as_mut()
                    .unwrap()
                    .deliver(Err(Failure::sip_rejection(
                        "ims_ussd_input_rejected",
                        response.status,
                    )));
                self.terminate_ussd(false).await?;
            }
            return Ok(());
        }
        if method != Method::INVITE
            || seq != 1
            || !same_via(&response.data.headers, &u.request.data.headers)
            || single(&response.data.headers, "From")? != single(&u.request.data.headers, "From")?
        {
            return Ok(());
        }
        let has_tag = single(&response.data.headers, "To")
            .ok()
            .and_then(tag)
            .is_some();
        let action =
            ussd_dialog::invite_response_action(response.status, u.dialog.is_some(), has_tag);
        match action {
            InviteResponseAction::IgnoreProvisional => return Ok(()),
            InviteResponseAction::Reject => {
                let ack = transaction(&u.request, Method::ACK, Some(&response));
                let acknowledged = match ack {
                    Ok(ack) => self.send(ack).await,
                    Err(error) => Err(error),
                };
                let u = self.ussd.as_mut().unwrap();
                u.finished = true;
                u.unknown = false;
                // The carrier's rejection is already known. Preserve the
                // independent ACK failure for the dispatcher/cleanup owner,
                // while completing the pending HTTP result without replay.
                u.deliver(Err(Failure::sip_rejection(
                    "ims_ussd_rejected",
                    response.status,
                )));
                return acknowledged;
            }
            InviteResponseAction::MissingRemoteTag => {
                let u = self.ussd.as_mut().unwrap();
                u.unknown = true;
                u.deliver(Err(Failure::known("ims_ussd_dialog_invalid")));
                return Ok(());
            }
            InviteResponseAction::Establish => {
                if response.data.headers.get("Contact").is_none() {
                    push(
                        &mut response.data.headers,
                        "Contact",
                        &format!("<{}>", u.request.uri),
                    )?;
                }
                let dialog = Dialog::from_initial_exchange(
                    DialogRole::UserAgentClient,
                    &u.request,
                    &response,
                )
                .map_err(|_| Failure::known("ims_ussd_dialog_invalid"))?;
                self.ussd.as_mut().unwrap().dialog = Some(dialog);
            }
            InviteResponseAction::AcknowledgeEstablished => {}
        }
        let via = self.via();
        let u = self.ussd.as_mut().unwrap();
        let ack = u
            .dialog
            .as_mut()
            .unwrap()
            .build_ack(via)
            .map_err(|_| Failure::known("ims_ussd_dialog_invalid"))?;
        let cancelled = u.cancelling || u.finished;
        self.send(ack).await?;
        if cancelled {
            let via = self.via();
            let u = self.ussd.as_mut().unwrap();
            if !u.bye_sent {
                let bye = u
                    .dialog
                    .as_mut()
                    .unwrap()
                    .build_request(Method::BYE, via, vec![])
                    .map_err(|_| Failure::known("ims_ussd_dialog_invalid"))?;
                u.bye_sent = true;
                self.send(bye).await?;
            }
            return Ok(());
        }
        loop {
            let pending = self.ussd.as_mut().unwrap().pending.pop_front();
            let Some((connection, request)) = pending else {
                break;
            };
            self.ussd_request(connection, request).await?;
        }
        Ok(())
    }
    async fn ussd_request(&mut self, connection: SipConnectionId, request: Request) -> Result<()> {
        let u = self.ussd.as_ref().unwrap();
        if u.finished {
            if request.method != Method::ACK {
                self.respond(
                    connection,
                    reply_for(
                        &request,
                        481,
                        "Call/Transaction Does Not Exist",
                        None,
                        vec![],
                        None,
                    )?,
                )
                .await?;
            }
            return Ok(());
        }
        let (seq, method) = cseq(&request.data.headers).unwrap_or((0, request.method.clone()));
        let remote_seq = u.dialog.as_ref().map(|d| d.remote_cseq()).unwrap_or(0);
        let local_matches = u.dialog.as_ref().is_some_and(|d| {
            single(&request.data.headers, "To").ok().and_then(tag) == Some(d.id.local_tag.as_str())
        });
        let remote_matches = u.dialog.as_ref().is_some_and(|d| {
            single(&request.data.headers, "From").ok().and_then(tag)
                == Some(d.id.remote_tag.as_str())
        });
        let package = single(&request.data.headers, "Info-Package").ok() == Some("g.3gpp.ussd");
        let action = ussd_dialog::inbound_request_action(
            u.dialog.is_some(),
            u.pending.len(),
            remote_matches,
            local_matches,
            request.method.as_str(),
            seq != 0,
            seq,
            remote_seq,
            package,
        );
        let rejection = match action {
            InboundRequestAction::QueuePending => {
                self.ussd
                    .as_mut()
                    .unwrap()
                    .pending
                    .push_back((connection, request));
                return Ok(());
            }
            InboundRequestAction::IgnoreAck => return Ok(()),
            InboundRequestAction::AcknowledgeDuplicate => Some((200, "OK")),
            InboundRequestAction::RejectServiceUnavailable => Some((503, "Service Unavailable")),
            InboundRequestAction::RejectDialogMissing => {
                Some((481, "Call/Transaction Does Not Exist"))
            }
            InboundRequestAction::RejectMethod => Some((405, "Method Not Allowed")),
            InboundRequestAction::RejectBadRequest => Some((400, "Bad Request")),
            InboundRequestAction::RejectInfoPackage => Some((469, "Bad Info Package")),
            InboundRequestAction::Accept => None,
        };
        if let Some((status, reason)) = rejection {
            return self
                .respond(
                    connection,
                    reply_for(&request, status, reason, None, vec![], None)?,
                )
                .await;
        }
        let decoded = if request.data.body.is_empty() {
            Ok(ussd_xml::UssdXmlDocument::default())
        } else {
            ussd_xml::decode_content(
                single(&request.data.headers, "Content-Type")?,
                &request.data.body,
            )
            .map_err(|_| Failure::known("ims_ussd_content_invalid"))
        };
        let decoded = match decoded {
            Ok(value) if value.error_code.is_empty() => value,
            _ => {
                self.respond(
                    connection,
                    reply_for(&request, 400, "Bad Request", None, vec![], None)?,
                )
                .await?;
                self.ussd
                    .as_mut()
                    .unwrap()
                    .deliver(Err(Failure::known("ims_ussd_content_invalid")));
                return self.terminate_ussd(false).await;
            }
        };
        self.ussd
            .as_mut()
            .unwrap()
            .dialog
            .as_mut()
            .unwrap()
            .accept_remote_cseq(seq, &method)
            .map_err(|_| Failure::known("ims_ussd_cseq_invalid"))?;
        self.respond(
            connection,
            reply_for(&request, 200, "OK", None, vec![], None)?,
        )
        .await?;
        let ended = request.method == Method::BYE;
        let u = self.ussd.as_mut().unwrap();
        u.waiting_input = !ended;
        u.finished = ended;
        u.deliver(Ok(UssdResult {
            session_id: u.id().into(),
            text: decoded.text,
            language: decoded.language,
            needs_input: !ended,
            complete: ended,
        }));
        Ok(())
    }
}
fn remove(headers: &mut Headers, names: &[&str]) {
    let mut result = Headers::default();
    for header in headers.iter() {
        if !names.iter().any(|name| header.is_named(name)) {
            result.push(header.clone());
        }
    }
    *headers = result;
}
fn transaction(invite: &Request, method: Method, response: Option<&Response>) -> Result<Request> {
    let mut request = invite.clone();
    request.method = method.clone();
    request.data.body.clear();
    remove(
        &mut request.data.headers,
        &[
            "CSeq",
            "Content-Type",
            "Content-Length",
            "Recv-Info",
            "Contact",
            "P-Preferred-Identity",
            "P-Preferred-Service",
            "Accept-Contact",
            "Accept",
        ],
    );
    push(
        &mut request.data.headers,
        "CSeq",
        &format!("1 {}", method.as_str()),
    )?;
    if let Some(response) = response {
        request.data.headers.set(
            Header::new("To", single(&response.data.headers, "To")?)
                .map_err(|_| Failure::known("ims_ussd_header_invalid"))?,
        );
    }
    Ok(request)
}
