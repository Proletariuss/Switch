use crate::Error;
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub(crate) const MAX_LINE: usize = 512;
const REQUEST_DOMAIN: &[u8] = b"switch-voice-supervisor/request/v1\0";
const RESPONSE_DOMAIN: &[u8] = b"switch-voice-supervisor/response/v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Status,
    Stop,
    Release,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Running,
    Stopping,
    Exited,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Exit {
    None,
    Code(i32),
    Signal(i32),
}
pub struct Response {
    state: State,
    pid: u32,
    exit: Exit,
}
impl Response {
    pub fn state(&self) -> State {
        self.state
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn exit(&self) -> Exit {
        self.exit
    }
}
impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("state", &self.state)
            .field("pid", &self.pid)
            .field("exit", &self.exit)
            .finish()
    }
}
impl Command {
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Status => "STATUS",
            Self::Stop => "STOP",
            Self::Release => "RELEASE",
        }
    }
    pub fn from_cli(value: &str) -> Result<Self, Error> {
        match value {
            "status" => Ok(Self::Status),
            "stop" => Ok(Self::Stop),
            "release" => Ok(Self::Release),
            _ => Err(Error::Protocol),
        }
    }
    fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "STATUS" => Ok(Self::Status),
            "STOP" => Ok(Self::Stop),
            "RELEASE" => Ok(Self::Release),
            _ => Err(Error::Protocol),
        }
    }
}
impl State {
    fn token(self) -> &'static str {
        match self {
            Self::Running => "RUNNING",
            Self::Stopping => "STOPPING",
            Self::Exited => "EXITED",
        }
    }
    fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "RUNNING" => Ok(Self::Running),
            "STOPPING" => Ok(Self::Stopping),
            "EXITED" => Ok(Self::Exited),
            _ => Err(Error::Protocol),
        }
    }
}
pub(crate) fn lower_hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn decode_hex<const N: usize>(value: &str) -> Result<[u8; N], Error> {
    if !lower_hex(value, N * 2) {
        return Err(Error::Protocol);
    }
    let mut bytes = [0; N];
    for (out, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let nibble = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        *out = nibble(pair[0]) * 16 + nibble(pair[1]);
    }
    Ok(bytes)
}
fn mac(domain: &[u8], unsigned: &str, key: &[u8]) -> Result<String, Error> {
    if key.len() != 32 {
        return Err(Error::Authentication);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| Error::Authentication)?;
    mac.update(domain);
    mac.update(unsigned.as_bytes());
    Ok(mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn authenticate(domain: &[u8], unsigned: &str, signature: &str, key: &[u8]) -> Result<(), Error> {
    if key.len() != 32 {
        return Err(Error::Authentication);
    }
    let signature = decode_hex::<32>(signature).map_err(|_| Error::Authentication)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| Error::Authentication)?;
    mac.update(domain);
    mac.update(unsigned.as_bytes());
    // hmac's verify_slice performs constant-time tag comparison.
    mac.verify_slice(&signature)
        .map_err(|_| Error::Authentication)
}
fn fields(line: &[u8]) -> Result<Vec<&str>, Error> {
    if line.len() > MAX_LINE
        || line.last() != Some(&b'\n')
        || !line[..line.len() - 1]
            .iter()
            .all(|b| (0x20..=0x7e).contains(b))
    {
        return Err(Error::Protocol);
    }
    let text = std::str::from_utf8(&line[..line.len() - 1]).map_err(|_| Error::Protocol)?;
    let fields: Vec<_> = text.split(' ').collect();
    if fields.iter().any(|field| field.is_empty()) {
        return Err(Error::Protocol);
    }
    Ok(fields)
}
pub(crate) fn valid_session(session: &str) -> bool {
    lower_hex(session, 32)
}
fn scope(session: &str, nonce: &str) -> Result<(), Error> {
    if lower_hex(session, 32) && lower_hex(nonce, 64) {
        Ok(())
    } else {
        Err(Error::Protocol)
    }
}
pub fn encode_request(
    session: &str,
    nonce: &str,
    command: Command,
    key: &[u8],
) -> Result<String, Error> {
    scope(session, nonce)?;
    let unsigned = format!("SVS1 REQUEST {session} {nonce} {}", command.token());
    Ok(format!(
        "{unsigned} {}\n",
        mac(REQUEST_DOMAIN, &unsigned, key)?
    ))
}
pub(crate) struct Request<'a> {
    pub(crate) nonce: &'a str,
    pub(crate) command: Command,
}
pub(crate) fn verify_request<'a>(
    line: &'a [u8],
    session: &str,
    key: &[u8],
) -> Result<Request<'a>, Error> {
    let f = fields(line)?;
    if f.len() != 6 || f[0] != "SVS1" || f[1] != "REQUEST" || f[2] != session {
        return Err(Error::Protocol);
    }
    scope(f[2], f[3])?;
    let unsigned = f[..5].join(" ");
    authenticate(REQUEST_DOMAIN, &unsigned, f[5], key)?;
    Ok(Request {
        nonce: f[3],
        command: Command::parse(f[4])?,
    })
}
fn decimal(value: &str) -> Result<i32, Error> {
    if value.is_empty()
        || value.len() > 10
        || !value.bytes().all(|b| b.is_ascii_digit())
        || value.len() > 1 && value.starts_with('0')
    {
        return Err(Error::Protocol);
    }
    value.parse().map_err(|_| Error::Protocol)
}
fn exit_text(exit: Exit) -> Result<String, Error> {
    match exit {
        Exit::None => Ok("NONE".into()),
        Exit::Code(code) if (0..=255).contains(&code) => Ok(format!("CODE:{code}")),
        Exit::Signal(signal) if (1..=127).contains(&signal) => Ok(format!("SIGNAL:{signal}")),
        _ => Err(Error::Protocol),
    }
}
fn valid_response(state: State, pid: u32, exit: Exit) -> Result<(), Error> {
    if pid <= 1 || pid > i32::MAX as u32 || (state == State::Exited) == (exit == Exit::None) {
        return Err(Error::Protocol);
    }
    exit_text(exit).map(|_| ())
}
pub fn encode_response(
    session: &str,
    nonce: &str,
    command: Command,
    state: State,
    pid: u32,
    exit: Exit,
    key: &[u8],
) -> Result<String, Error> {
    scope(session, nonce)?;
    valid_response(state, pid, exit)?;
    let unsigned = format!(
        "SVS1 RESPONSE {session} {nonce} {} {} {pid} {}",
        command.token(),
        state.token(),
        exit_text(exit)?
    );
    Ok(format!(
        "{unsigned} {}\n",
        mac(RESPONSE_DOMAIN, &unsigned, key)?
    ))
}
pub fn verify_response(
    line: &[u8],
    session: &str,
    nonce: &str,
    command: Command,
    key: &[u8],
) -> Result<Response, Error> {
    scope(session, nonce)?;
    let f = fields(line)?;
    if f.len() != 9
        || f[0] != "SVS1"
        || f[1] != "RESPONSE"
        || f[2] != session
        || f[3] != nonce
        || f[4] != command.token()
    {
        return Err(Error::Protocol);
    }
    authenticate(RESPONSE_DOMAIN, &f[..8].join(" "), f[8], key)?;
    let state = State::parse(f[5])?;
    let pid = decimal(f[6])? as u32;
    let exit = if f[7] == "NONE" {
        Exit::None
    } else if let Some(v) = f[7].strip_prefix("CODE:") {
        Exit::Code(decimal(v)?)
    } else if let Some(v) = f[7].strip_prefix("SIGNAL:") {
        Exit::Signal(decimal(v)?)
    } else {
        return Err(Error::Protocol);
    };
    valid_response(state, pid, exit)?;
    Ok(Response { state, pid, exit })
}

#[cfg(test)]
mod tests {
    use super::*;
    const SESSION: &str = "00112233445566778899aabbccddeeff";
    const NONCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    #[test]
    fn authenticated_fields_and_domains_are_not_interchangeable() {
        let key = [7u8; 32];
        let request = encode_request(SESSION, NONCE, Command::Stop, &key).unwrap();
        assert_eq!(
            verify_request(request.as_bytes(), SESSION, &key)
                .unwrap()
                .command,
            Command::Stop
        );
        assert!(verify_request(request.as_bytes(), SESSION, &[8; 32]).is_err());
        let response = encode_response(
            SESSION,
            NONCE,
            Command::Stop,
            State::Exited,
            123,
            Exit::Code(0),
            &key,
        )
        .unwrap();
        let result =
            verify_response(response.as_bytes(), SESSION, NONCE, Command::Stop, &key).unwrap();
        assert_eq!(result.state(), State::Exited);
        assert!(
            verify_response(response.as_bytes(), SESSION, NONCE, Command::Status, &key).is_err()
        );
        assert!(
            verify_response(
                response.as_bytes(),
                SESSION,
                &"b".repeat(64),
                Command::Stop,
                &key
            )
            .is_err()
        );
        assert!(verify_response(request.as_bytes(), SESSION, NONCE, Command::Stop, &key).is_err());
        assert!(verify_request(response.as_bytes(), SESSION, &key).is_err());
    }
    #[test]
    fn malformed_or_inconsistent_signed_responses_are_rejected() {
        for line in [
            b"".as_slice(),
            b"SVS1\r\n",
            b"SVS1\nextra\n",
            b"SVS1  REQUEST\n",
            b"SVS1\0\n",
        ] {
            assert!(verify_request(line, SESSION, &[7; 32]).is_err());
        }
        assert!(
            encode_response(
                SESSION,
                NONCE,
                Command::Stop,
                State::Running,
                123,
                Exit::Code(0),
                &[7; 32]
            )
            .is_err()
        );
        assert!(
            encode_response(
                SESSION,
                NONCE,
                Command::Stop,
                State::Exited,
                123,
                Exit::None,
                &[7; 32]
            )
            .is_err()
        );
        assert!(
            encode_response(
                SESSION,
                NONCE,
                Command::Stop,
                State::Exited,
                1,
                Exit::Code(0),
                &[7; 32]
            )
            .is_err()
        );
    }
}
