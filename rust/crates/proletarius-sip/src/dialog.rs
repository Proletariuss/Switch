// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! SIP dialog identity, sequence ownership and route planning.
//!
//! A dialog is a value owned by one runtime task. It contains no socket,
//! callback, mutex or background worker; transaction and transport layers can
//! therefore consume the same state without sharing mutable network objects.

use std::fmt;

use crate::{Header, Headers, Method, Request, Response, Uri};

pub const MAX_CSEQ: u32 = 2_147_483_647;
pub const MAX_ROUTE_SET: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogError {
    MissingHeader(&'static str),
    InvalidHeader(&'static str),
    InvalidResponse(u16),
    InvalidState(DialogState),
    InvalidCseq,
    RouteCapacity,
}

impl fmt::Display for DialogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeader(name) => write!(formatter, "dialog requires {name} header"),
            Self::InvalidHeader(name) => write!(formatter, "dialog has invalid {name} header"),
            Self::InvalidResponse(status) => {
                write!(formatter, "response status {status} cannot create a dialog")
            }
            Self::InvalidState(state) => write!(formatter, "invalid dialog state: {state:?}"),
            Self::InvalidCseq => formatter.write_str("dialog CSeq is out of order"),
            Self::RouteCapacity => formatter.write_str("dialog route set exceeds configured limit"),
        }
    }
}

impl std::error::Error for DialogError {}

pub type DialogResult<T> = std::result::Result<T, DialogError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogRole {
    UserAgentClient,
    UserAgentServer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogState {
    Early,
    Established,
    Confirmed,
    Terminated,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DialogId {
    pub call_id: String,
    pub local_tag: String,
    pub remote_tag: String,
}

impl fmt::Display for DialogId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{};{};{}",
            self.call_id, self.local_tag, self.remote_tag
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cseq {
    pub sequence: u32,
    pub method: Method,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogRoutePlan {
    pub request_uri: Uri,
    pub routes: Vec<Uri>,
    pub next_hop: Uri,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dialog {
    pub id: DialogId,
    pub role: DialogRole,
    pub state: DialogState,
    pub local_uri: Uri,
    pub remote_uri: Uri,
    pub remote_target: Uri,
    route_set: Vec<Uri>,
    local_cseq: u32,
    local_invite_cseq: u32,
    remote_cseq: u32,
    remote_invite_cseq: u32,
}

impl Dialog {
    pub fn from_initial_exchange(
        role: DialogRole,
        request: &Request,
        response: &Response,
    ) -> DialogResult<Self> {
        let state = match response.status {
            101..=199 => DialogState::Early,
            200..=299 => DialogState::Established,
            status => return Err(DialogError::InvalidResponse(status)),
        };
        let call_id = required_header(&request.data.headers, "Call-ID")?
            .trim()
            .to_owned();
        if call_id.is_empty() {
            return Err(DialogError::InvalidHeader("Call-ID"));
        }
        let response_call_id = required_header(&response.data.headers, "Call-ID")?.trim();
        if !response_call_id.is_empty() && response_call_id != call_id {
            return Err(DialogError::InvalidHeader("Call-ID"));
        }
        let request_cseq = parse_cseq(&request.data.headers)?;
        let response_cseq = parse_cseq(&response.data.headers)?;
        if request_cseq != response_cseq {
            return Err(DialogError::InvalidCseq);
        }

        let request_from = parse_address_header(&request.data.headers, "From")?;
        let response_from = parse_address_header(&response.data.headers, "From")?;
        let response_to = parse_address_header(&response.data.headers, "To")?;
        let request_from_tag = required_tag(&request.data.headers, "From")?;
        let response_to_tag = required_tag(&response.data.headers, "To")?;

        let (local_uri, remote_uri, local_tag, remote_tag, remote_target, mut route_set) =
            match role {
                DialogRole::UserAgentClient => (
                    request_from,
                    response_to,
                    request_from_tag,
                    response_to_tag,
                    parse_contact(&response.data.headers)?,
                    parse_route_set(&response.data.headers)?,
                ),
                DialogRole::UserAgentServer => (
                    response_to,
                    response_from,
                    response_to_tag,
                    request_from_tag,
                    parse_contact(&request.data.headers)?,
                    parse_route_set(&request.data.headers)?,
                ),
            };
        if role == DialogRole::UserAgentClient {
            route_set.reverse();
        }
        if route_set.len() > MAX_ROUTE_SET {
            return Err(DialogError::RouteCapacity);
        }

        let (remote_cseq, remote_invite_cseq) = match role {
            // The peer has its own CSeq space.  For an outbound INVITE the
            // first in-dialog request from the peer may legitimately start
            // at 1, independently of our INVITE sequence.
            DialogRole::UserAgentClient => (0, request_cseq.sequence),
            DialogRole::UserAgentServer => (request_cseq.sequence, request_cseq.sequence),
        };

        Ok(Self {
            id: DialogId {
                call_id,
                local_tag,
                remote_tag,
            },
            role,
            state,
            local_uri,
            remote_uri,
            remote_target,
            route_set,
            local_cseq: request_cseq.sequence,
            local_invite_cseq: request_cseq.sequence,
            remote_cseq,
            remote_invite_cseq,
        })
    }

    /// Apply a later response from the same initial exchange without losing
    /// CSeq progress already consumed by PRACK or another early-dialog request.
    pub fn update_from_initial_response(
        &mut self,
        request: &Request,
        response: &Response,
    ) -> DialogResult<()> {
        let mut updated = Self::from_initial_exchange(self.role, request, response)?;
        if updated.id != self.id {
            return Err(DialogError::InvalidHeader("dialog identifier"));
        }
        updated.local_cseq = updated.local_cseq.max(self.local_cseq);
        updated.local_invite_cseq = updated.local_invite_cseq.max(self.local_invite_cseq);
        updated.remote_cseq = updated.remote_cseq.max(self.remote_cseq);
        updated.remote_invite_cseq = updated.remote_invite_cseq.max(self.remote_invite_cseq);
        *self = updated;
        Ok(())
    }

    #[must_use]
    pub fn route_set(&self) -> &[Uri] {
        &self.route_set
    }

    #[must_use]
    pub const fn local_cseq(&self) -> u32 {
        self.local_cseq
    }

    #[must_use]
    pub const fn remote_cseq(&self) -> u32 {
        self.remote_cseq
    }

    pub fn next_local_cseq(&mut self, method: Method) -> DialogResult<Cseq> {
        let sequence = if matches!(method, Method::Ack | Method::Cancel) {
            self.local_invite_cseq
        } else {
            if self.local_cseq >= MAX_CSEQ {
                return Err(DialogError::InvalidCseq);
            }
            self.local_cseq += 1;
            if method == Method::Invite {
                self.local_invite_cseq = self.local_cseq;
            }
            self.local_cseq
        };
        Ok(Cseq { sequence, method })
    }

    pub fn accept_remote_cseq(&mut self, sequence: u32, method: &Method) -> DialogResult<()> {
        if sequence > MAX_CSEQ {
            return Err(DialogError::InvalidCseq);
        }
        if matches!(method, Method::Ack | Method::Cancel) {
            if sequence != self.remote_invite_cseq {
                return Err(DialogError::InvalidCseq);
            }
            return Ok(());
        }
        if sequence <= self.remote_cseq {
            return Err(DialogError::InvalidCseq);
        }
        self.remote_cseq = sequence;
        if method == &Method::Invite {
            self.remote_invite_cseq = sequence;
        }
        Ok(())
    }

    pub fn establish(&mut self) -> DialogResult<()> {
        match self.state {
            DialogState::Early | DialogState::Established => {
                self.state = DialogState::Established;
                Ok(())
            }
            DialogState::Confirmed | DialogState::Terminated => {
                Err(DialogError::InvalidState(self.state))
            }
        }
    }

    pub fn confirm(&mut self) -> DialogResult<()> {
        match self.state {
            DialogState::Established | DialogState::Confirmed => {
                self.state = DialogState::Confirmed;
                Ok(())
            }
            DialogState::Early | DialogState::Terminated => {
                Err(DialogError::InvalidState(self.state))
            }
        }
    }

    pub fn terminate(&mut self) {
        self.state = DialogState::Terminated;
    }

    pub fn route_plan(&self) -> DialogResult<DialogRoutePlan> {
        if self.state == DialogState::Terminated {
            return Err(DialogError::InvalidState(self.state));
        }
        let Some(first) = self.route_set.first() else {
            return Ok(DialogRoutePlan {
                request_uri: self.remote_target.clone(),
                routes: Vec::new(),
                next_hop: self.remote_target.clone(),
            });
        };
        if first.uri_params.contains("lr") {
            return Ok(DialogRoutePlan {
                request_uri: self.remote_target.clone(),
                routes: self.route_set.clone(),
                next_hop: first.clone(),
            });
        }

        let mut routes = Vec::with_capacity(self.route_set.len());
        routes.extend(self.route_set.iter().skip(1).cloned());
        routes.push(self.remote_target.clone());
        Ok(DialogRoutePlan {
            request_uri: first.clone(),
            routes,
            next_hop: first.clone(),
        })
    }

    /// Build an in-dialog request while leaving transport and transaction
    /// ownership with the caller.
    ///
    /// The caller supplies the complete Via value because the transaction
    /// layer owns branch generation and transport selection.  The dialog
    /// contributes only its immutable identity, route plan and local CSeq.
    /// The body is moved into the request so a media or signaling caller can
    /// hand over its already-sized buffer without an intermediate copy.
    pub fn build_request(
        &mut self,
        method: Method,
        via: impl Into<String>,
        body: Vec<u8>,
    ) -> DialogResult<Request> {
        let plan = self.route_plan()?;
        let via = via.into();
        if via.trim().is_empty() {
            return Err(DialogError::InvalidHeader("Via"));
        }
        let cseq = self.next_local_cseq(method.clone())?;
        let mut headers = Headers::default();
        headers.push(Header::new("Via", via).map_err(|_| DialogError::InvalidHeader("Via"))?);
        headers.push(
            Header::new(
                "From",
                format!("<{}>;tag={}", self.local_uri, self.id.local_tag),
            )
            .map_err(|_| DialogError::InvalidHeader("From"))?,
        );
        headers.push(
            Header::new(
                "To",
                format!("<{}>;tag={}", self.remote_uri, self.id.remote_tag),
            )
            .map_err(|_| DialogError::InvalidHeader("To"))?,
        );
        headers.push(
            Header::new("Call-ID", self.id.call_id.clone())
                .map_err(|_| DialogError::InvalidHeader("Call-ID"))?,
        );
        headers.push(
            Header::new("CSeq", format!("{} {}", cseq.sequence, method.as_str()))
                .map_err(|_| DialogError::InvalidHeader("CSeq"))?,
        );
        headers.push(
            Header::new("Max-Forwards", "70")
                .map_err(|_| DialogError::InvalidHeader("Max-Forwards"))?,
        );
        for route in plan.routes {
            headers.push(
                Header::new("Route", format!("<{route}>"))
                    .map_err(|_| DialogError::InvalidHeader("Route"))?,
            );
        }
        Ok(Request {
            method,
            uri: plan.request_uri,
            data: crate::MessageData::new("SIP/2.0", headers, body),
        })
    }

    /// Build the ACK for the dialog's initial INVITE transaction.
    pub fn build_ack(&mut self, via: impl Into<String>) -> DialogResult<Request> {
        self.build_request(Method::Ack, via, Vec::new())
    }

    /// Build a BYE request.  BYE is only valid after a dialog has been
    /// established; requiring that state here prevents an accidental
    /// transaction from being sent with an incomplete dialog identity.
    pub fn build_bye(&mut self, via: impl Into<String>, body: Vec<u8>) -> DialogResult<Request> {
        if !matches!(
            self.state,
            DialogState::Established | DialogState::Confirmed
        ) {
            return Err(DialogError::InvalidState(self.state));
        }
        self.build_request(Method::Bye, via, body)
    }

    /// Build a reliable provisional response acknowledgement (PRACK).
    /// `rack` must be the complete RAck header value from the corresponding
    /// reliable provisional response.
    pub fn build_prack(
        &mut self,
        via: impl Into<String>,
        rack: impl Into<String>,
        body: Vec<u8>,
    ) -> DialogResult<Request> {
        if self.state == DialogState::Terminated {
            return Err(DialogError::InvalidState(self.state));
        }
        let rack = rack.into();
        if rack.trim().is_empty() {
            return Err(DialogError::InvalidHeader("RAck"));
        }
        let mut request = self.build_request(Method::Prack, via, body)?;
        request
            .data
            .headers
            .push(Header::new("RAck", rack).map_err(|_| DialogError::InvalidHeader("RAck"))?);
        Ok(request)
    }
}

fn required_header<'a>(headers: &'a Headers, name: &'static str) -> DialogResult<&'a str> {
    headers
        .get(name)
        .map(Header::value)
        .ok_or(DialogError::MissingHeader(name))
}

fn parse_cseq(headers: &Headers) -> DialogResult<Cseq> {
    let value = required_header(headers, "CSeq")?;
    let mut fields = value.split_whitespace();
    let sequence = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value <= MAX_CSEQ)
        .ok_or(DialogError::InvalidHeader("CSeq"))?;
    let method = fields
        .next()
        .map(str::to_owned)
        .ok_or(DialogError::InvalidHeader("CSeq"))?;
    if fields.next().is_some() {
        return Err(DialogError::InvalidHeader("CSeq"));
    }
    Ok(Cseq {
        sequence,
        method: Method::new(method).map_err(|_| DialogError::InvalidHeader("CSeq"))?,
    })
}

fn required_tag(headers: &Headers, name: &'static str) -> DialogResult<String> {
    let value = required_header(headers, name)?;
    split_header_parameters(value)
        .find_map(|parameter| {
            let (key, value) = parameter.split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("tag")
                .then(|| trim_quotes(value.trim()).to_owned())
        })
        .filter(|tag| !tag.is_empty())
        .ok_or(DialogError::InvalidHeader(name))
}

fn parse_address_header(headers: &Headers, name: &'static str) -> DialogResult<Uri> {
    parse_name_address(required_header(headers, name)?)
        .map_err(|_| DialogError::InvalidHeader(name))
}

fn parse_contact(headers: &Headers) -> DialogResult<Uri> {
    let contact = required_header(headers, "Contact")?;
    let uri = parse_name_address(contact).map_err(|_| DialogError::InvalidHeader("Contact"))?;
    if uri.wildcard {
        return Err(DialogError::InvalidHeader("Contact"));
    }
    Ok(uri)
}

fn parse_route_set(headers: &Headers) -> DialogResult<Vec<Uri>> {
    let mut routes = Vec::new();
    for header in headers.all("Record-Route") {
        for value in split_header_values(header.value()) {
            if routes.len() >= MAX_ROUTE_SET {
                return Err(DialogError::RouteCapacity);
            }
            routes.push(
                parse_name_address(value)
                    .map_err(|_| DialogError::InvalidHeader("Record-Route"))?,
            );
        }
    }
    Ok(routes)
}

fn parse_name_address(value: &str) -> crate::Result<Uri> {
    let value = value.trim();
    if let Some(start) = value.find('<') {
        let end = value[start + 1..]
            .find('>')
            .map(|index| start + 1 + index)
            .ok_or(crate::Error::InvalidUri)?;
        return Uri::parse(value[start + 1..end].trim());
    }
    Uri::parse(value)
}

fn split_header_parameters(value: &str) -> impl Iterator<Item = &str> {
    let offset = value.find('>').map_or(0, |index| index + 1);
    value[offset..].split(';').skip(1)
}

fn split_header_values(value: &str) -> impl Iterator<Item = &str> {
    let mut quoted = false;
    let mut angle_depth = 0_u8;
    value.split(move |character| match character {
        '"' => {
            quoted = !quoted;
            false
        }
        '<' if !quoted => {
            angle_depth = angle_depth.saturating_add(1);
            false
        }
        '>' if !quoted => {
            angle_depth = angle_depth.saturating_sub(1);
            false
        }
        ',' => !quoted && angle_depth == 0,
        _ => false,
    })
}

fn trim_quotes(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}
