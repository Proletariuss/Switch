//! In-place REGISTER transactions on the retained primary association.
//! No transport, dialog or media ownership is moved by successful renewal.
use super::*;
use crate::registration_coordinator::{granted_registration_lease, registration_request_lease};
use proletarius_ims::{
    SipResponse,
    aka_authorization::AuthorizationRequest,
    aka_exchange::{SimAnswer, answer_challenge},
};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::watch;

pub type RefreshAuthenticate = Arc<
    dyn Fn(
            Zeroizing<[u8; 16]>,
            Zeroizing<[u8; 16]>,
        ) -> Pin<Box<dyn Future<Output = io::Result<SimAnswer>> + Send>>
        + Send
        + Sync,
>;
type Authentication = Pin<Box<dyn Future<Output = io::Result<(String, Zeroizing<String>)>> + Send>>;
const RESPONSE_BUDGET: Duration = Duration::from_secs(30);

pub(crate) struct Configuration {
    seed: Zeroizing<Vec<u8>>,
    username: Zeroizing<String>,
    authenticate: RefreshAuthenticate,
    expiry: Instant,
    due: Instant,
    stop: watch::Receiver<bool>,
    secured: bool,
}
impl Configuration {
    pub(crate) fn renewal_deadline(&self) -> Instant {
        self.due
    }
    pub(crate) fn new(
        seed: &[u8],
        username: String,
        authenticate: RefreshAuthenticate,
        expiry: Instant,
        due: Instant,
        stop: watch::Receiver<bool>,
        secured: bool,
    ) -> io::Result<Self> {
        if username.is_empty() || registration_request_lease(seed).is_none() {
            return Err(failed("REGISTER refresh configuration invalid"));
        }
        let due = due.min(
            Instant::now()
                + expiry
                    .saturating_duration_since(Instant::now())
                    .mul_f64(0.75),
        );
        Ok(Self {
            seed: Zeroizing::new(seed.to_vec()),
            username: Zeroizing::new(username),
            authenticate,
            expiry,
            due,
            stop,
            secured,
        })
    }
}
pub(super) struct Refresh {
    config: Configuration,
    ticket: Option<ResponseTicket>,
    writing: bool,
    response_deadline: Option<Instant>,
    authentication: Option<Authentication>,
    authorization: Option<(String, Zeroizing<String>)>,
    challenges: u8,
}
impl Refresh {
    pub(super) fn new(config: Configuration) -> Self {
        Self {
            config,
            ticket: None,
            writing: false,
            response_deadline: None,
            authentication: None,
            authorization: None,
            challenges: 0,
        }
    }
    pub(super) fn owns(&self, ticket: ResponseTicket) -> bool {
        self.ticket == Some(ticket)
    }
    pub(super) fn expiry(&self) -> Instant {
        self.config.expiry
    }
    pub(super) fn writing(&self) -> bool {
        self.writing
    }
    pub(super) fn deadline(&self) -> Instant {
        let work = if self.authentication.is_some() {
            Instant::now() + Duration::from_millis(10)
        } else if let Some(d) = self.response_deadline {
            d
        } else {
            self.config.due
        };
        work.min(self.config.expiry)
    }
    pub(super) async fn tick(
        &mut self,
        session: &mut RegisteredImsSession,
        responses: &mut ResponseRegistry,
    ) -> io::Result<()> {
        let now = Instant::now();
        if now >= self.config.expiry {
            return Err(failed("IMS registration lease expired"));
        }
        if *self.config.stop.borrow() || self.config.stop.has_changed().is_err() {
            return Err(failed("IMS registration refresh stopped"));
        }
        if self.response_deadline.is_some_and(|d| now >= d) {
            return Err(failed("IMS REGISTER refresh response timed out"));
        }
        if let Some(auth) = &mut self.authentication {
            let result =
                tokio::select! {biased; value=auth=>Some(value), _=std::future::ready(())=>None};
            if let Some(result) = result {
                self.authorization = Some(result?);
                self.authentication = None;
            } else {
                return Ok(());
            }
        }
        if self.ticket.is_none() && now >= self.config.due {
            let request = self.request()?;
            let ticket = responses.reserve_registration(&request, RESPONSE_BUDGET)?;
            let wire = Zeroizing::new(Message::Request(request).encode());
            if let Err(error) = session.queue_heartbeat(&wire) {
                responses.cancel(ticket);
                return Err(error);
            }
            self.config.seed = wire;
            self.ticket = Some(ticket);
            self.writing = true;
        }
        if self.writing {
            tokio::select! {biased; _=tokio::time::sleep_until(self.config.expiry)=>return Err(failed("IMS registration lease expired during write")), result=session.finish_heartbeat_write()=>result?};
            self.writing = false;
            let deadline = (Instant::now() + RESPONSE_BUDGET).min(self.config.expiry);
            self.response_deadline = Some(deadline);
            responses.arm(self.ticket.unwrap(), deadline)?;
        }
        Ok(())
    }
    fn request(&mut self) -> io::Result<Request> {
        let (message, used) = Parser::default()
            .parse(&self.config.seed, false)
            .map_err(|_| failed("REGISTER seed parse failed"))?;
        if used != self.config.seed.len() {
            return Err(failed("REGISTER seed trailing bytes"));
        }
        let Message::Request(mut request) = message else {
            return Err(failed("REGISTER seed is not a request"));
        };
        let headers = &request.data.headers;
        let cseq = headers
            .get("CSeq")
            .and_then(|h| h.value().split_whitespace().next())
            .and_then(|v| v.parse::<u32>().ok())
            .and_then(|v| v.checked_add(1))
            .ok_or_else(|| failed("REGISTER CSeq exhausted"))?;
        let mut entropy = [0u8; 16];
        getrandom::fill(&mut entropy).map_err(|_| failed("REGISTER entropy unavailable"))?;
        let branch = format!(
            "z9hG4bK{}",
            entropy
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let mut next = Headers::default();
        for h in headers.iter() {
            if ["Authorization", "Proxy-Authorization", "Security-Client"]
                .iter()
                .any(|name| h.is_named(name))
            {
                continue;
            }
            let value = if h.is_named("CSeq") {
                format!("{cseq} REGISTER")
            } else if h.is_named("Via") {
                let mut pieces = h
                    .value()
                    .split(';')
                    .filter(|p| !p.trim_start().to_ascii_lowercase().starts_with("branch="))
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                pieces.push(format!("branch={branch}"));
                pieces.join(";")
            } else {
                h.value().to_owned()
            };
            next.push(Header::new(h.name(), value).map_err(|_| failed("REGISTER header invalid"))?);
        }
        if let Some((name, value)) = self.authorization.take() {
            next.push(
                Header::new(name, value.as_str())
                    .map_err(|_| failed("REGISTER authorization invalid"))?,
            );
        }
        request.data.headers = next;
        Ok(request)
    }
    pub(super) fn response(
        &mut self,
        message: &RegisteredSipMessage,
        session: &mut RegisteredImsSession,
    ) -> io::Result<()> {
        let RegisteredSipMessage::Response {
            status,
            reason,
            version,
            headers,
            body,
        } = message
        else {
            return Ok(());
        };
        if *status < 200 {
            return Ok(());
        }
        if Instant::now() >= self.config.expiry {
            return Err(failed("IMS registration lease expired before response"));
        }
        self.ticket = None;
        self.response_deadline = None;
        // New security negotiation needs overlapping SA ownership, which this
        // retained transport cannot provide. Never ignore or falsely accept it.
        if headers.get("Security-Server").is_some() {
            return Err(failed(
                "REGISTER refresh requires unsupported security association replacement",
            ));
        }
        let response = Response {
            status: *status,
            reason: reason.clone(),
            data: MessageData {
                version: version.clone(),
                headers: headers.clone(),
                body: body.to_vec(),
            },
        };
        let wire = Zeroizing::new(Message::Response(response).encode());
        let parsed =
            SipResponse::parse(&wire).map_err(|_| failed("REGISTER response parse failed"))?;
        if *status == 200 {
            let lease = registration_request_lease(&self.config.seed)
                .ok_or_else(|| failed("REGISTER lease request invalid"))?;
            let seconds = granted_registration_lease(&lease, &parsed)
                .filter(|s| *s > 0)
                .ok_or_else(|| failed("REGISTER refresh did not grant a valid lease"))?;
            let delay = crate::registration_renewal_delay_seconds(seconds)
                .ok_or_else(|| failed("REGISTER lease cannot be renewed"))?;
            let now = Instant::now();
            self.config.expiry = now + Duration::from_secs(seconds);
            self.config.due =
                now + Duration::from_secs(delay).min(Duration::from_secs(seconds).mul_f64(0.75));
            self.challenges = 0;
            session.refreshed(seconds, self.config.expiry, self.config.due);
            return Ok(());
        }
        if !matches!(*status, 401 | 407) || self.challenges >= 3 {
            return Err(failed("REGISTER refresh rejected"));
        }
        self.challenges += 1;
        let (message, _) = Parser::default()
            .parse(&self.config.seed, false)
            .map_err(|_| failed("REGISTER seed invalid"))?;
        let Message::Request(request) = message else {
            unreachable!()
        };
        let uri = request.uri.to_string();
        let username = self.config.username.clone();
        let authenticate = self.config.authenticate.clone();
        let stop = self.config.stop.clone();
        let deadline = self.config.expiry.min(Instant::now() + RESPONSE_BUDGET);
        let secured = self.config.secured;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| failed("REGISTER authentication entropy unavailable"))?;
        let cnonce = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        self.authentication = Some(Box::pin(async move {
            let answer = answer_challenge(
                &parsed,
                &AuthorizationRequest {
                    username: &username,
                    method: "REGISTER",
                    uri: &uri,
                    body: &[],
                    cnonce: &cnonce,
                    nonce_count: 1,
                },
                deadline,
                stop,
                move |rand, autn| authenticate(rand, autn),
            )
            .await
            .map_err(|_| failed("REGISTER refresh AKA failed"))?;
            let header = answer.header();
            let mut value = Zeroizing::new(header.value().to_owned());
            if secured {
                value.push_str(",integrity-protected=yes");
            }
            Ok((header.name().to_owned(), value))
        }));
        Ok(())
    }
}
fn failed(message: &'static str) -> io::Error {
    io::Error::other(message)
}
