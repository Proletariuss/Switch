// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! Small, ownership-explicit SIP core.
//!
//! The crate deliberately stops at the protocol and transaction boundary.
//! Socket ownership belongs to a future transport crate; this keeps parsing,
//! message construction and the RFC transaction key usable by the Switch
//! daemon without importing a network runtime.

use std::collections::BTreeMap;
use std::fmt;

mod dialog;
mod transaction;

pub use dialog::*;
pub use transaction::*;

pub const MAX_MESSAGE_SIZE: usize = 65_535;
pub const MAX_MESSAGES_PER_PUSH: usize = 16;
pub const MAX_URI_SIZE: usize = 8_192;
pub const MAX_URI_PARAMS: usize = 64;
pub const MAX_HEADER_COUNT: usize = 128;
pub const MAX_HEADER_LINE_SIZE: usize = 8_192;
pub const MAX_HEADER_VALUE_SIZE: usize = 16_384;
pub const UDP_SAFE_MESSAGE_SIZE: usize = 1_300;
pub const UDP_MTU_MARGIN: usize = 200;

/// RFC 3261 requires congestion-controlled transport when a request is close
/// to a known path MTU, or larger than 1300 bytes when the path MTU is unknown.
#[must_use]
pub const fn requires_stream_transport(message_size: usize, path_mtu: Option<usize>) -> bool {
    let datagram_limit = match path_mtu {
        Some(mtu) => mtu.saturating_sub(UDP_MTU_MARGIN),
        None => UDP_SAFE_MESSAGE_SIZE,
    };
    message_size > datagram_limit
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    Incomplete,
    MessageTooLarge,
    InvalidStartLine,
    InvalidHeader,
    InvalidUri,
    InvalidContentLength,
    InvalidUtf8,
    TransactionCapacity,
    OutputCapacity,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Incomplete => "incomplete SIP message",
            Self::MessageTooLarge => "SIP message exceeds configured limit",
            Self::InvalidStartLine => "invalid SIP start line",
            Self::InvalidHeader => "invalid SIP header",
            Self::InvalidUri => "invalid SIP URI",
            Self::InvalidContentLength => "invalid SIP Content-Length",
            Self::InvalidUtf8 => "SIP text is not valid UTF-8",
            Self::TransactionCapacity => "SIP transaction table is full",
            Self::OutputCapacity => "SIP output batch capacity is exhausted",
        };
        formatter.write_str(text)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Method {
    Invite,
    Ack,
    Cancel,
    Bye,
    Register,
    Options,
    Subscribe,
    Notify,
    Refer,
    Info,
    Message,
    Prack,
    Update,
    Publish,
    Other(String),
}

impl Method {
    pub const INVITE: Self = Self::Invite;
    pub const ACK: Self = Self::Ack;
    pub const CANCEL: Self = Self::Cancel;
    pub const BYE: Self = Self::Bye;
    pub const REGISTER: Self = Self::Register;
    pub const OPTIONS: Self = Self::Options;
    pub const SUBSCRIBE: Self = Self::Subscribe;
    pub const NOTIFY: Self = Self::Notify;
    pub const REFER: Self = Self::Refer;
    pub const INFO: Self = Self::Info;
    pub const MESSAGE: Self = Self::Message;
    pub const PRACK: Self = Self::Prack;
    pub const UPDATE: Self = Self::Update;
    pub const PUBLISH: Self = Self::Publish;

    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.bytes().any(|byte| byte <= b' ' || byte == 0x7f) {
            return Err(Error::InvalidStartLine);
        }
        Ok(match value.as_str() {
            "INVITE" => Self::Invite,
            "ACK" => Self::Ack,
            "CANCEL" => Self::Cancel,
            "BYE" => Self::Bye,
            "REGISTER" => Self::Register,
            "OPTIONS" => Self::Options,
            "SUBSCRIBE" => Self::Subscribe,
            "NOTIFY" => Self::Notify,
            "REFER" => Self::Refer,
            "INFO" => Self::Info,
            "MESSAGE" => Self::Message,
            "PRACK" => Self::Prack,
            "UPDATE" => Self::Update,
            "PUBLISH" => Self::Publish,
            _ => Self::Other(value),
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Invite => "INVITE",
            Self::Ack => "ACK",
            Self::Cancel => "CANCEL",
            Self::Bye => "BYE",
            Self::Register => "REGISTER",
            Self::Options => "OPTIONS",
            Self::Subscribe => "SUBSCRIBE",
            Self::Notify => "NOTIFY",
            Self::Refer => "REFER",
            Self::Info => "INFO",
            Self::Message => "MESSAGE",
            Self::Prack => "PRACK",
            Self::Update => "UPDATE",
            Self::Publish => "PUBLISH",
            Self::Other(value) => value,
        }
    }
}

impl From<Method> for String {
    fn from(value: Method) -> Self {
        value.as_str().to_owned()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Params(Vec<(String, String)>);

impl Params {
    pub fn parse(value: &str, separator: char) -> Self {
        let mut params = Vec::new();
        for item in value.split(separator).filter(|item| !item.is_empty()) {
            let (key, value) = item.split_once('=').unwrap_or((item, ""));
            params.push((key.trim().to_owned(), value.trim().to_owned()));
        }
        Self(params)
    }

    fn parse_checked(value: &str, separator: char, maximum: usize) -> Result<Self> {
        let mut params = Vec::with_capacity(value.len().min(maximum));
        for item in value.split(separator).filter(|item| !item.is_empty()) {
            if params.len() >= maximum {
                return Err(Error::InvalidUri);
            }
            let (key, value) = item.split_once('=').unwrap_or((item, ""));
            let key = key.trim();
            if key.is_empty() {
                return Err(Error::InvalidUri);
            }
            params.push((key.to_owned(), value.trim().to_owned()));
        }
        Ok(Self(params))
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    }

    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    #[must_use]
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    pub fn push(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.push((key.into(), value.into()));
    }

    /// Replace every case-insensitive occurrence with one canonical value.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        self.0.retain(|(name, _)| !name.eq_ignore_ascii_case(&key));
        self.0.push((key, value.into()));
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn render(&self, separator: char, output: &mut String) {
        for (index, (key, value)) in self.0.iter().enumerate() {
            if index != 0 {
                output.push(separator);
            }
            output.push_str(key);
            if !value.is_empty() {
                output.push('=');
                output.push_str(value);
            }
        }
    }

    fn render_bytes(&self, separator: u8, output: &mut Vec<u8>) {
        for (index, (key, value)) in self.0.iter().enumerate() {
            if index != 0 {
                output.push(separator);
            }
            output.extend_from_slice(key.as_bytes());
            if !value.is_empty() {
                output.push(b'=');
                output.extend_from_slice(value.as_bytes());
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Uri {
    pub scheme: String,
    pub wildcard: bool,
    pub user: String,
    pub password: String,
    pub host: String,
    pub port: Option<u16>,
    pub uri_params: Params,
    pub headers: Params,
}

impl Uri {
    pub fn new(scheme: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            scheme: scheme.into(),
            wildcard: false,
            user: String::new(),
            password: String::new(),
            host: host.into(),
            port: None,
            uri_params: Params::default(),
            headers: Params::default(),
        }
    }

    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() || input.len() > MAX_URI_SIZE {
            return Err(Error::InvalidUri);
        }
        let (scheme, mut rest) = input.split_once(':').unwrap_or(("sip", input));
        if scheme.is_empty() {
            return Err(Error::InvalidUri);
        }
        if rest == "*" {
            let mut uri = Self::new(scheme, "");
            uri.wildcard = true;
            return Ok(uri);
        }

        let headers = if let Some((base, query)) = rest.split_once('?') {
            rest = base;
            Params::parse_checked(query, '&', MAX_URI_PARAMS)?
        } else {
            Params::default()
        };
        let uri_params = if let Some((base, params)) = rest.split_once(';') {
            rest = base;
            Params::parse_checked(params, ';', MAX_URI_PARAMS)?
        } else {
            Params::default()
        };

        let (userinfo, hostport) = rest.rsplit_once('@').unwrap_or(("", rest));
        let (user, password) = userinfo
            .split_once(':')
            .map_or((userinfo, ""), |(user, password)| (user, password));
        let (host, port) = parse_host_port(hostport)?;
        if host.is_empty() {
            return Err(Error::InvalidUri);
        }
        Ok(Self {
            scheme: scheme.to_owned(),
            wildcard: false,
            user: user.to_owned(),
            password: password.to_owned(),
            host,
            port,
            uri_params,
            headers,
        })
    }

    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.scheme.eq_ignore_ascii_case("sips")
    }

    #[must_use]
    pub fn endpoint(&self) -> String {
        let mut value = String::new();
        if !self.user.is_empty() {
            value.push_str(&self.user);
            value.push('@');
        }
        value.push_str(&self.host);
        if let Some(port) = self.port {
            value.push(':');
            value.push_str(&port.to_string());
        }
        value
    }

    #[must_use]
    pub fn address(&self) -> String {
        let scheme = if self.scheme.is_empty() {
            "sip"
        } else {
            &self.scheme
        };
        format!("{scheme}:{}", self.endpoint())
    }

    fn render_into(&self, output: &mut Vec<u8>) {
        let scheme = if self.scheme.is_empty() {
            "sip"
        } else {
            &self.scheme
        };
        output.extend_from_slice(scheme.as_bytes());
        output.push(b':');
        if self.wildcard {
            output.push(b'*');
            return;
        }
        if !self.user.is_empty() {
            output.extend_from_slice(self.user.as_bytes());
            if !self.password.is_empty() {
                output.push(b':');
                output.extend_from_slice(self.password.as_bytes());
            }
            output.push(b'@');
        }
        if self.host.contains(':') && !self.host.starts_with('[') {
            output.push(b'[');
            output.extend_from_slice(self.host.as_bytes());
            output.push(b']');
        } else {
            output.extend_from_slice(self.host.as_bytes());
        }
        if let Some(port) = self.port {
            output.push(b':');
            push_decimal(output, port);
        }
        if !self.uri_params.is_empty() {
            output.push(b';');
            self.uri_params.render_bytes(b';', output);
        }
        if !self.headers.is_empty() {
            output.push(b'?');
            self.headers.render_bytes(b'&', output);
        }
    }
}

fn push_decimal(output: &mut Vec<u8>, mut value: u16) {
    let mut digits = [0_u8; 5];
    let mut cursor = digits.len();
    loop {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    output.extend_from_slice(&digits[cursor..]);
}

impl fmt::Display for Uri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.scheme.is_empty() {
            "sip"
        } else {
            &self.scheme
        };
        write!(formatter, "{scheme}:")?;
        if self.wildcard {
            return formatter.write_str("*");
        }
        if !self.user.is_empty() {
            write!(formatter, "{}", self.user)?;
            if !self.password.is_empty() {
                write!(formatter, ":{}", self.password)?;
            }
            formatter.write_str("@")?;
        }
        if self.host.contains(':') && !self.host.starts_with('[') {
            write!(formatter, "[{}]", self.host)?;
        } else {
            formatter.write_str(&self.host)?;
        }
        if let Some(port) = self.port {
            write!(formatter, ":{port}")?;
        }
        if !self.uri_params.is_empty() {
            formatter.write_str(";")?;
            let mut value = String::new();
            self.uri_params.render(';', &mut value);
            formatter.write_str(&value)?;
        }
        if !self.headers.is_empty() {
            formatter.write_str("?")?;
            let mut value = String::new();
            self.headers.render('&', &mut value);
            formatter.write_str(&value)?;
        }
        Ok(())
    }
}

fn parse_host_port(value: &str) -> Result<(String, Option<u16>)> {
    if value.starts_with('[') {
        let end = value.find(']').ok_or(Error::InvalidUri)?;
        let host = value[1..end].to_owned();
        let tail = &value[end + 1..];
        if tail.is_empty() {
            return Ok((host, None));
        }
        let port = tail
            .strip_prefix(':')
            .ok_or(Error::InvalidUri)?
            .parse()
            .map_err(|_| Error::InvalidUri)?;
        return Ok((host, Some(port)));
    }
    if value.matches(':').count() == 1 {
        let (host, port) = value.rsplit_once(':').ok_or(Error::InvalidUri)?;
        if let Ok(port) = port.parse() {
            return Ok((host.to_owned(), Some(port)));
        }
    }
    Ok((value.to_owned(), None))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    name: String,
    value: String,
}

impl Header {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if name.trim().is_empty() || name.bytes().any(|byte| byte <= b' ' || byte == b':') {
            return Err(Error::InvalidHeader);
        }
        let value = value.into();
        if value.len() > MAX_HEADER_VALUE_SIZE
            || value.bytes().any(|byte| byte == b'\r' || byte == b'\n')
        {
            return Err(Error::InvalidHeader);
        }
        Ok(Self {
            name,
            value: value.trim().to_owned(),
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn is_named(&self, name: &str) -> bool {
        header_name_eq(&self.name, name)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Headers {
    items: Vec<Header>,
}

impl Headers {
    pub fn push(&mut self, header: Header) {
        self.items.push(header);
    }

    pub fn prepend(&mut self, header: Header) {
        self.items.insert(0, header);
    }

    pub fn set(&mut self, header: Header) {
        if let Some(existing) = self
            .items
            .iter_mut()
            .find(|existing| existing.is_named(header.name()))
        {
            *existing = header;
        } else {
            self.push(header);
        }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Header> {
        self.items.iter().find(|header| header.is_named(name))
    }

    pub fn all(&self, name: &str) -> impl Iterator<Item = &Header> {
        self.items
            .iter()
            .filter(move |header| header.is_named(name))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Header> {
        self.items.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn content_length(&self) -> Result<usize> {
        let mut length = None;
        for header in self.all("Content-Length") {
            let value = header
                .value
                .parse::<usize>()
                .map_err(|_| Error::InvalidContentLength)?;
            if let Some(previous) = length
                && previous != value
            {
                return Err(Error::InvalidContentLength);
            }
            length = Some(value);
        }
        Ok(length.unwrap_or(0))
    }
}

fn header_name_eq(actual: &str, requested: &str) -> bool {
    if actual.eq_ignore_ascii_case(requested) {
        return true;
    }
    let actual = actual.as_bytes();
    let requested = requested.as_bytes();
    if actual.len() == 1 {
        return compact_header_name(actual[0]).is_some_and(|long| {
            std::str::from_utf8(requested)
                .is_ok_and(|requested| long.eq_ignore_ascii_case(requested))
        });
    }
    requested.len() == 1
        && compact_header_name(requested[0]).is_some_and(|long| {
            std::str::from_utf8(actual).is_ok_and(|actual| long.eq_ignore_ascii_case(actual))
        })
}

fn compact_header_name(name: u8) -> Option<&'static str> {
    match name.to_ascii_lowercase() {
        b'v' => Some("Via"),
        b'f' => Some("From"),
        b't' => Some("To"),
        b'i' => Some("Call-ID"),
        b'm' => Some("Contact"),
        b'l' => Some("Content-Length"),
        b'c' => Some("Content-Type"),
        b's' => Some("Subject"),
        b'k' => Some("Supported"),
        b'e' => Some("Content-Encoding"),
        b'o' => Some("Event"),
        b'r' => Some("Refer-To"),
        b'b' => Some("Referred-By"),
        b'u' => Some("Allow-Events"),
        b'd' => Some("Request-Disposition"),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageData {
    pub version: String,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl MessageData {
    #[must_use]
    pub fn new(version: impl Into<String>, headers: Headers, body: Vec<u8>) -> Self {
        Self {
            version: version.into(),
            headers,
            body,
        }
    }

    fn render_into(&self, output: &mut Vec<u8>) {
        let mut has_length = false;
        for header in self.headers.iter() {
            if header.is_named("Content-Length") {
                if has_length {
                    continue;
                }
                has_length = true;
                output.extend_from_slice(b"Content-Length: ");
                output.extend_from_slice(self.body.len().to_string().as_bytes());
            } else {
                output.extend_from_slice(header.name.as_bytes());
                output.extend_from_slice(b": ");
                output.extend_from_slice(header.value.as_bytes());
            }
            output.extend_from_slice(b"\r\n");
        }
        if !has_length {
            output.extend_from_slice(b"Content-Length: ");
            output.extend_from_slice(self.body.len().to_string().as_bytes());
            output.extend_from_slice(b"\r\n");
        }
        output.extend_from_slice(b"\r\n");
        output.extend_from_slice(&self.body);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    pub method: Method,
    pub uri: Uri,
    pub data: MessageData,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub data: MessageData,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    Request(Request),
    Response(Response),
}

impl Message {
    #[must_use]
    pub fn headers(&self) -> &Headers {
        match self {
            Self::Request(request) => &request.data.headers,
            Self::Response(response) => &response.data.headers,
        }
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        match self {
            Self::Request(request) => &request.data.body,
            Self::Response(response) => &response.data.body,
        }
    }

    #[must_use]
    pub fn method(&self) -> Option<&Method> {
        match self {
            Self::Request(request) => Some(&request.method),
            Self::Response(_) => None,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(self.body().len().saturating_add(512));
        self.encode_into(&mut output);
        output
    }

    /// Encode into caller-owned storage, preserving its allocation for the
    /// next SIP datagram.
    pub fn encode_into(&self, output: &mut Vec<u8>) {
        output.clear();
        match self {
            Self::Request(request) => {
                output.extend_from_slice(request.method.as_str().as_bytes());
                output.push(b' ');
                request.uri.render_into(output);
                output.push(b' ');
                output.extend_from_slice(request.data.version.as_bytes());
                output.extend_from_slice(b"\r\n");
                request.data.render_into(output);
            }
            Self::Response(response) => {
                output.extend_from_slice(response.data.version.as_bytes());
                output.push(b' ');
                output.extend_from_slice(response.status.to_string().as_bytes());
                output.push(b' ');
                output.extend_from_slice(response.reason.as_bytes());
                output.extend_from_slice(b"\r\n");
                response.data.render_into(output);
            }
        }
    }
}

pub struct Parser {
    pub max_message_size: usize,
}

impl Default for Parser {
    fn default() -> Self {
        Self {
            max_message_size: MAX_MESSAGE_SIZE,
        }
    }
}

impl Parser {
    pub fn parse(&self, data: &[u8], stream: bool) -> Result<(Message, usize)> {
        let max_message_size = self.max_message_size.min(MAX_MESSAGE_SIZE);
        if data.len() > max_message_size {
            return Err(Error::MessageTooLarge);
        }
        let mut offset: usize = 0;
        if stream {
            while data
                .get(offset..offset.saturating_add(2))
                .is_some_and(|value| value == b"\r\n")
            {
                offset += 2;
            }
        }
        let header_end = find_double_crlf(&data[offset..]).ok_or(Error::Incomplete)?;
        let header_end = offset + header_end + 4;
        let header_bytes = &data[offset..header_end];
        let mut lines = header_bytes.split(|byte| *byte == b'\n');
        let start = lines.next().ok_or(Error::InvalidStartLine)?;
        let start = trim_cr(start);
        let mut headers = Headers::default();
        let mut current_name = None::<String>;
        let mut current_value = String::new();
        for raw in lines {
            let line = trim_cr(raw);
            if line.is_empty() {
                break;
            }
            if line
                .first()
                .is_some_and(|byte| *byte == b' ' || *byte == b'\t')
            {
                if current_name.is_none() {
                    return Err(Error::InvalidHeader);
                }
                if line.len() > MAX_HEADER_LINE_SIZE
                    || current_value.len().saturating_add(line.len() + 1) > MAX_HEADER_VALUE_SIZE
                {
                    return Err(Error::InvalidHeader);
                }
                current_value.push(' ');
                current_value.push_str(parse_text(line)?.trim());
                continue;
            }
            if let Some(name) = current_name.take() {
                if headers.len() >= MAX_HEADER_COUNT {
                    return Err(Error::InvalidHeader);
                }
                headers.push(Header::new(name, std::mem::take(&mut current_value))?);
            }
            let colon = line
                .iter()
                .position(|byte| *byte == b':')
                .ok_or(Error::InvalidHeader)?;
            let (name, value) = (&line[..colon], &line[colon + 1..]);
            if line.len() > MAX_HEADER_LINE_SIZE {
                return Err(Error::InvalidHeader);
            }
            let name = parse_text(name)?.trim().to_owned();
            if name.is_empty() {
                return Err(Error::InvalidHeader);
            }
            current_name = Some(name);
            current_value = parse_text(value)?.trim().to_owned();
        }
        if let Some(name) = current_name {
            if headers.len() >= MAX_HEADER_COUNT {
                return Err(Error::InvalidHeader);
            }
            headers.push(Header::new(name, current_value)?);
        }
        let body_length = headers.content_length()?;
        let total = header_end
            .checked_add(body_length)
            .ok_or(Error::MessageTooLarge)?;
        if total > max_message_size {
            return Err(Error::MessageTooLarge);
        }
        if data.len() < total {
            return Err(Error::Incomplete);
        }
        let body = data[header_end..total].to_vec();
        let message = parse_start_line(start, headers, body)?;
        Ok((message, total))
    }
}

fn parse_text(data: &[u8]) -> Result<&str> {
    std::str::from_utf8(data).map_err(|_| Error::InvalidUtf8)
}

fn trim_cr(data: &[u8]) -> &[u8] {
    data.strip_suffix(b"\r").unwrap_or(data)
}

fn find_double_crlf(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_start_line(line: &[u8], headers: Headers, body: Vec<u8>) -> Result<Message> {
    let line = parse_text(line)?;
    if let Some(rest) = line.strip_prefix("SIP/") {
        let mut parts = rest.splitn(3, ' ');
        let version = format!("SIP/{}", parts.next().ok_or(Error::InvalidStartLine)?);
        let status = parts
            .next()
            .ok_or(Error::InvalidStartLine)?
            .parse()
            .map_err(|_| Error::InvalidStartLine)?;
        let reason = parts.next().unwrap_or_default().to_owned();
        return Ok(Message::Response(Response {
            status,
            reason,
            data: MessageData::new(version, headers, body),
        }));
    }
    let mut parts = line.splitn(3, ' ');
    let method = Method::new(parts.next().ok_or(Error::InvalidStartLine)?)?;
    let uri = Uri::parse(parts.next().ok_or(Error::InvalidStartLine)?)?;
    let version = parts.next().ok_or(Error::InvalidStartLine)?.to_owned();
    if version != "SIP/2.0" {
        return Err(Error::InvalidStartLine);
    }
    Ok(Message::Request(Request {
        method,
        uri,
        data: MessageData::new(version, headers, body),
    }))
}

pub struct StreamParser {
    parser: Parser,
    buffer: Vec<u8>,
    consumed: usize,
}

impl StreamParser {
    pub fn new(max_message_size: usize) -> Self {
        Self {
            parser: Parser {
                max_message_size: max_message_size.min(MAX_MESSAGE_SIZE),
            },
            buffer: Vec::with_capacity(4096),
            consumed: 0,
        }
    }

    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Message>> {
        let mut messages = Vec::with_capacity(MAX_MESSAGES_PER_PUSH);
        self.push_into(data, &mut messages, MAX_MESSAGES_PER_PUSH)?;
        Ok(messages)
    }

    /// Feed bytes into the parser and append at most `max_messages` complete
    /// messages to caller-owned storage. The parser retains any complete
    /// frames beyond that bound for the next call, so a burst cannot grow an
    /// output vector without the caller's explicit capacity.
    pub fn push_into(
        &mut self,
        data: &[u8],
        output: &mut Vec<Message>,
        max_messages: usize,
    ) -> Result<usize> {
        output.clear();
        let max_messages = max_messages.min(MAX_MESSAGES_PER_PUSH);
        if max_messages == 0 {
            return Ok(0);
        }
        if output.capacity() < max_messages {
            return Err(Error::OutputCapacity);
        }
        self.compact();
        if self.buffer.len().saturating_add(data.len()) > self.parser.max_message_size {
            return Err(Error::MessageTooLarge);
        }
        self.buffer.extend_from_slice(data);
        while output.len() < max_messages {
            // RFC 5626 CRLF keepalives share long-lived IMS TCP flows with
            // SIP messages. Consume complete keepalive pairs between frames,
            // while retaining a lone CR for the next fragmented read.
            while self.buffer[self.consumed..].starts_with(b"\r\n") {
                self.consumed += 2;
            }
            if self.consumed == self.buffer.len() {
                break;
            }
            match self.parser.parse(&self.buffer[self.consumed..], true) {
                Ok((message, consumed)) => {
                    self.consumed += consumed;
                    output.push(message);
                }
                Err(Error::Incomplete) => break,
                Err(error) => {
                    self.compact();
                    return Err(error);
                }
            }
        }
        self.compact();
        Ok(output.len())
    }

    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.len().saturating_sub(self.consumed)
    }

    fn compact(&mut self) {
        if self.consumed == 0 {
            return;
        }
        if self.consumed == self.buffer.len() {
            self.buffer.clear();
            self.consumed = 0;
            return;
        }
        let remaining = self.buffer.len() - self.consumed;
        self.buffer.copy_within(self.consumed.., 0);
        self.buffer.truncate(remaining);
        self.consumed = 0;
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TransactionKey {
    pub branch: String,
    pub method: Method,
    pub sent_by: String,
}

impl TransactionKey {
    pub fn from_request(request: &Request) -> Option<Self> {
        let via = request.data.headers.get("Via")?;
        let mut fields = via.value.split(';');
        let sent_by = fields.next()?.trim().strip_prefix("SIP/2.0/")?;
        let (transport, sent_by) = sent_by.split_once(' ')?;
        let branch = fields
            .find_map(|field| field.trim().strip_prefix("branch="))?
            .to_owned();
        Some(Self {
            branch,
            method: request.method.clone(),
            sent_by: format!("{transport} {sent_by}"),
        })
    }
}

#[must_use]
pub fn header_value<'a>(message: &'a Message, name: &str) -> Option<&'a str> {
    message.headers().get(name).map(Header::value)
}

#[must_use]
pub fn call_id(message: &Message) -> Option<&str> {
    header_value(message, "Call-ID")
}

#[must_use]
pub fn cseq(message: &Message) -> Option<(u32, &str)> {
    let value = header_value(message, "CSeq")?;
    let mut fields = value.split_whitespace();
    Some((fields.next()?.parse().ok()?, fields.next()?))
}

#[must_use]
pub fn header_tag<'a>(message: &'a Message, name: &str) -> Option<&'a str> {
    let value = header_value(message, name)?;
    value.split(';').skip(1).find_map(|item| {
        let (key, value) = item.trim().split_once('=')?;
        key.eq_ignore_ascii_case("tag").then_some(value)
    })
}

#[must_use]
pub fn dialog_id(call_id: &str, local_tag: &str, remote_tag: &str) -> String {
    format!("{call_id};{local_tag};{remote_tag}")
}

pub struct TransactionTable<T> {
    capacity: usize,
    items: BTreeMap<TransactionKey, T>,
}

impl<T> TransactionTable<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            items: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: TransactionKey, value: T) -> Result<Option<T>> {
        if !self.items.contains_key(&key) && self.items.len() >= self.capacity {
            return Err(Error::TransactionCapacity);
        }
        Ok(self.items.insert(key, value))
    }

    pub fn get(&self, key: &TransactionKey) -> Option<&T> {
        self.items.get(key)
    }

    pub fn remove(&mut self, key: &TransactionKey) -> Option<T> {
        self.items.remove(key)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
