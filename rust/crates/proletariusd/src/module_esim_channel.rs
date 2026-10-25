//! Bounded AT+CCHO/CGLA/CCHC bridge for module eUICC APDUs.
//!
//! This adapter translates the modem's AT logical-channel interface into the
//! existing Rust APDU transport used by `proletarius-euicc`. The caller must
//! hold the module-radio reservation and the USB AT owner's complete
//! transaction admission for the channel's full lifetime.

use std::fmt;
use std::time::Duration;

use proletarius_sim::ApduTransport;
use zeroize::Zeroize;

const OPEN_TIMEOUT: Duration = Duration::from_secs(8);
const APDU_TIMEOUT: Duration = Duration::from_secs(15);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_AID_BYTES: usize = 32;
const MAX_APDU_BYTES: usize = 65_535;
const MAX_RESPONSE_BYTES: usize = 4_098;
const MAX_AT_RESPONSE_BYTES: usize = MAX_RESPONSE_BYTES * 2 + 128;

/// One serialized AT command. Implementations must return the complete modem
/// response or an error and must never retry a submitted command.
pub(crate) trait AtCommandTransport {
    type Error;

    fn command(
        &mut self,
        command: &str,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<String, Self::Error>;
}

#[derive(Debug)]
pub(crate) enum Error<E> {
    Transport(E),
    InvalidAid,
    InvalidApdu,
    NoChannel,
    ModemRejected,
    MalformedResponse,
    ChannelCleanupFailed,
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "USB AT transport failed: {error}"),
            Self::InvalidAid => formatter.write_str("eUICC application AID is invalid"),
            Self::InvalidApdu => formatter.write_str("eUICC APDU is invalid or too large"),
            Self::NoChannel => formatter.write_str("eUICC logical channel is not open"),
            Self::ModemRejected => formatter.write_str("modem rejected the eUICC AT command"),
            Self::MalformedResponse => {
                formatter.write_str("modem returned a malformed eUICC response")
            }
            Self::ChannelCleanupFailed => {
                formatter.write_str("modem opened an eUICC channel that could not be closed")
            }
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for Error<E> {}

/// A single AT logical channel. Dropping an open instance does not issue
/// hidden I/O; the owner must explicitly call `close` before releasing its
/// transaction lease so cleanup errors remain observable.
pub(crate) struct ModuleEuiccChannel<'a, T: ?Sized> {
    transport: &'a mut T,
    channel: Option<u8>,
}

impl<'a, T: AtCommandTransport + ?Sized> ModuleEuiccChannel<'a, T> {
    pub(crate) fn open(transport: &'a mut T, aid: &[u8]) -> Result<Self, Error<T::Error>> {
        if aid.is_empty() || aid.len() > MAX_AID_BYTES {
            return Err(Error::InvalidAid);
        }
        let mut aid_hex = encode_hex(aid);
        let command = format!("AT+CCHO=\"{aid_hex}\"");
        aid_hex.zeroize();
        let mut response = transport
            .command(&command, OPEN_TIMEOUT, MAX_AT_RESPONSE_BYTES)
            .map_err(Error::Transport)?;
        if has_modem_error(&response) {
            response.zeroize();
            return Err(Error::ModemRejected);
        }
        let channel = parse_channel(&response);
        let accepted = channel.filter(|_| has_ok(&response));
        response.zeroize();
        let Some(channel) = accepted else {
            if let Some(channel) = channel {
                let command = format!("AT+CCHC={channel}");
                let mut cleanup = transport
                    .command(&command, CLOSE_TIMEOUT, 1024)
                    .map_err(|_| Error::ChannelCleanupFailed)?;
                let cleaned = !has_modem_error(&cleanup) && has_ok(&cleanup);
                cleanup.zeroize();
                if !cleaned {
                    return Err(Error::ChannelCleanupFailed);
                }
            }
            return Err(Error::MalformedResponse);
        };
        Ok(Self {
            transport,
            channel: Some(channel),
        })
    }

    pub(crate) fn channel(&self) -> Option<u8> {
        self.channel
    }

    /// Close once and mark the channel unusable even if CCHC fails. The APDU
    /// must not be replayed after an uncertain close result.
    pub(crate) fn close(&mut self) -> Result<(), Error<T::Error>> {
        let channel = self.channel.take().ok_or(Error::NoChannel)?;
        let command = format!("AT+CCHC={channel}");
        let mut response = self
            .transport
            .command(&command, CLOSE_TIMEOUT, 1024)
            .map_err(Error::Transport)?;
        let accepted = !has_modem_error(&response) && has_ok(&response);
        response.zeroize();
        if accepted {
            Ok(())
        } else {
            Err(Error::ChannelCleanupFailed)
        }
    }

    pub(crate) fn into_inner(self) -> &'a mut T {
        self.transport
    }
}

impl<T: AtCommandTransport + ?Sized> ApduTransport for ModuleEuiccChannel<'_, T> {
    type Error = Error<T::Error>;

    fn transmit(&mut self, apdu: &[u8]) -> Result<Vec<u8>, Self::Error> {
        let channel = self.channel.ok_or(Error::NoChannel)?;
        if apdu.is_empty() || apdu.len() > MAX_APDU_BYTES {
            return Err(Error::InvalidApdu);
        }

        let mut apdu_hex = encode_hex(apdu);
        let mut command = format!("AT+CGLA={channel},{},\"{apdu_hex}\"", apdu_hex.len());
        apdu_hex.zeroize();
        let response_result = self
            .transport
            .command(&command, APDU_TIMEOUT, MAX_AT_RESPONSE_BYTES);
        command.zeroize();
        let mut response = response_result.map_err(Error::Transport)?;
        if has_modem_error(&response) {
            response.zeroize();
            return Err(Error::ModemRejected);
        }
        let data = parse_cgla(&response).filter(|_| has_ok(&response));
        response.zeroize();
        data.ok_or(Error::MalformedResponse)
    }
}

fn has_modem_error(response: &str) -> bool {
    response.lines().any(|line| {
        let line = line.trim();
        line.eq_ignore_ascii_case("ERROR")
            || line.starts_with("+CME ERROR")
            || line.starts_with("+CMS ERROR")
    })
}

fn has_ok(response: &str) -> bool {
    response
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case("OK"))
}

fn parse_channel(response: &str) -> Option<u8> {
    let mut found = None;
    for line in response.lines().map(str::trim) {
        let Some(value) = line.strip_prefix("+CCHO:") else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        let channel = value.trim().parse::<u8>().ok()?;
        // ISO/IEC 7816 logical channel numbers range from 1 through 19.
        if !(1..=19).contains(&channel) {
            return None;
        }
        found = Some(channel);
    }
    found
}

fn parse_cgla(response: &str) -> Option<Vec<u8>> {
    let mut found = None;
    for line in response.lines().map(str::trim) {
        let Some(value) = line.strip_prefix("+CGLA:") else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        let (declared_length, encoded) = value.trim().split_once(',')?;
        let declared_length = declared_length.trim().parse::<usize>().ok()?;
        if declared_length > MAX_RESPONSE_BYTES * 2 {
            return None;
        }
        let encoded = encoded.trim().trim_matches('"');
        if encoded.is_empty() || encoded.len() > MAX_RESPONSE_BYTES * 2 || encoded.len() % 2 != 0 {
            return None;
        }
        let bytes = decode_hex(encoded)?;
        found = Some(bytes);
    }
    found
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

fn decode_hex(encoded: &str) -> Option<Vec<u8>> {
    let bytes = encoded.as_bytes();
    if bytes.len() % 2 != 0 {
        return None;
    }
    bytes
        .chunks_exact(2)
        .map(|pair| Some((hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?))
        .collect()
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'A'..=b'F' => Some(value - b'A' + 10),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    enum FakeError {
        Transport,
    }

    impl fmt::Display for FakeError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("fixture transport failure")
        }
    }

    struct FakeTransport {
        replies: VecDeque<Result<String, FakeError>>,
        commands: Vec<(String, Duration, usize)>,
    }

    impl AtCommandTransport for FakeTransport {
        type Error = FakeError;

        fn command(
            &mut self,
            command: &str,
            timeout: Duration,
            max_response_bytes: usize,
        ) -> Result<String, Self::Error> {
            self.commands
                .push((command.to_owned(), timeout, max_response_bytes));
            self.replies.pop_front().expect("fixture response")
        }
    }

    fn fake(replies: impl IntoIterator<Item = Result<&'static str, FakeError>>) -> FakeTransport {
        FakeTransport {
            replies: replies
                .into_iter()
                .map(|reply| reply.map(str::to_owned))
                .collect(),
            commands: Vec::new(),
        }
    }

    #[test]
    fn logical_channel_round_trips_apdu_and_closes_once() {
        let aid = [0xa0, 0x00, 0x00, 0x05, 0x59];
        let mut transport = fake([
            Ok("+CCHO: 3\r\nOK\r\n"),
            Ok("+CGLA: 4,9000\r\nOK\r\n"),
            Ok("OK\r\n"),
        ]);
        let mut channel = ModuleEuiccChannel::open(&mut transport, &aid).unwrap();
        assert_eq!(channel.channel(), Some(3));
        assert_eq!(
            channel.transmit(&[0x00, 0x84, 0x00, 0x00]).unwrap(),
            vec![0x90, 0x00]
        );
        channel.close().unwrap();
        assert_eq!(channel.channel(), None);

        assert_eq!(
            transport
                .commands
                .iter()
                .map(|entry| entry.0.as_str())
                .collect::<Vec<_>>(),
            [
                "AT+CCHO=\"A000000559\"",
                "AT+CGLA=3,8,\"00840000\"",
                "AT+CCHC=3",
            ]
        );
        assert_eq!(transport.commands[0].1, OPEN_TIMEOUT);
        assert_eq!(transport.commands[1].1, APDU_TIMEOUT);
        assert_eq!(transport.commands[2].1, CLOSE_TIMEOUT);
    }

    #[test]
    fn malformed_or_duplicate_channel_responses_fail_closed() {
        for response in [
            "+CCHO: 0\r\nOK\r\n",
            "+CCHO: 20\r\nOK\r\n",
            "+CCHO: 1\r\n+CCHO: 2\r\nOK\r\n",
            "OK\r\n",
        ] {
            let mut transport = fake([Ok(response)]);
            assert!(matches!(
                ModuleEuiccChannel::open(&mut transport, &[0xa0]),
                Err(Error::MalformedResponse)
            ));
        }

        let mut transport = fake([Ok("+CCHO: 4\r\n"), Ok("OK\r\n")]);
        assert!(matches!(
            ModuleEuiccChannel::open(&mut transport, &[0xa0]),
            Err(Error::MalformedResponse)
        ));
    }

    #[test]
    fn apdu_errors_are_not_retried_and_channel_close_remains_single_use() {
        let mut transport = fake([
            Ok("+CCHO: 1\r\nOK\r\n"),
            Ok("+CGLA: 3,90XZ\r\nOK\r\n"),
            Ok("ERROR\r\n"),
        ]);
        let mut channel = ModuleEuiccChannel::open(&mut transport, &[0xa0]).unwrap();
        assert!(matches!(
            channel.transmit(&[0x00, 0x84, 0x00, 0x00]),
            Err(Error::MalformedResponse)
        ));
        assert_eq!(channel.into_inner().commands.len(), 2);

        let mut transport = fake([Ok("+CCHO: 1\r\nOK\r\n"), Ok("ERROR\r\n")]);
        let mut channel = ModuleEuiccChannel::open(&mut transport, &[0xa0]).unwrap();
        assert!(matches!(channel.close(), Err(Error::ChannelCleanupFailed)));
        assert!(matches!(channel.close(), Err(Error::NoChannel)));

        let mut transport = fake([Ok("+CCHO: 2\r\nOK\r\n"), Err(FakeError::Transport)]);
        let mut channel = ModuleEuiccChannel::open(&mut transport, &[0xa0]).unwrap();
        assert!(matches!(
            channel.transmit(&[0x00, 0x84, 0x00, 0x00]),
            Err(Error::Transport(FakeError::Transport))
        ));
        assert_eq!(channel.into_inner().commands.len(), 2);
    }

    #[test]
    fn validates_aid_and_response_parser_bounds() {
        let mut transport = fake([Ok("+CCHO: 1\r\nOK\r\n")]);
        assert!(matches!(
            ModuleEuiccChannel::open(&mut transport, &[]),
            Err(Error::InvalidAid)
        ));

        let mut transport = fake([Ok("+CCHO: 1\r\nOK\r\n")]);
        let aid = vec![0xa0; MAX_AID_BYTES + 1];
        assert!(matches!(
            ModuleEuiccChannel::open(&mut transport, &aid),
            Err(Error::InvalidAid)
        ));
        assert_eq!(
            parse_cgla("+CGLA: 4,9000\r\nOK\r\n"),
            Some(vec![0x90, 0x00])
        );
        assert_eq!(parse_cgla("+CGLA: 2,900\r\nOK\r\n"), None);
        assert_eq!(parse_cgla("+CGLA: 2,90G0\r\nOK\r\n"), None);
        assert_eq!(parse_channel("+CCHO: 5\r\nOK\r\n"), Some(5));
        assert!(has_ok("+CCHC: 1\r\nOK\r\n"));
    }
}
