//! Bridge the reader's serialized APDU transport to the reusable GSMA eUICC
//! protocol crate. This adapter does not open a reader or acquire ownership;
//! callers must hold the normal reader operation lock for the full session.

use std::fmt;

use proletarius_ccid::{CardTransport, Error as CcidError, channel::OwnedChannel};
use proletarius_sim::ApduTransport;

const MAX_APDU_RESPONSE_BYTES: usize = 4_098;

#[derive(Debug)]
pub enum Error<E> {
    Apdu(E),
    Protocol(CcidError),
}

impl<E> From<CcidError> for Error<E> {
    fn from(error: CcidError) -> Self {
        Self::Protocol(error)
    }
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Apdu(error) => write!(formatter, "reader APDU transport failed: {error}"),
            Self::Protocol(error) => write!(formatter, "reader card protocol failed: {error}"),
        }
    }
}

impl<E> std::error::Error for Error<E> where E: std::error::Error + 'static {}

/// Adapts one already-owned Switch APDU transport for eUICC operations.
/// Short APDUs are used deliberately: the eUICC crate applies its own 254-byte
/// segmentation before commands reach PC/SC.
pub struct ApduCardTransport<T>(T);

/// One eUICC logical channel over an already-owned APDU transport.
pub type OwnedEuiccChannel<T> = OwnedChannel<ApduCardTransport<T>>;

impl<T> ApduCardTransport<T> {
    pub const fn new(transport: T) -> Self {
        Self(transport)
    }

    pub fn into_inner(self) -> T {
        self.0
    }

    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: ApduTransport> CardTransport for ApduCardTransport<T> {
    type Error = Error<T::Error>;

    fn transmit(&mut self, command: &[u8], response: &mut Vec<u8>) -> Result<(), Self::Error> {
        response.clear();
        let reply = self.0.transmit(command).map_err(Error::Apdu)?;
        if reply.len() > MAX_APDU_RESPONSE_BYTES {
            return Err(CcidError::Invalid("APDU response exceeds the transport limit").into());
        }
        response.extend_from_slice(&reply);
        Ok(())
    }
}

/// Open and select an eUICC application on a newly allocated logical
/// channel. Existing channels are never probed or closed; failed SELECT only
/// closes the channel allocated by this call. The caller must keep the
/// transport's hardware lease for the full lifetime of the returned channel.
pub fn open_euicc_channel<T: ApduTransport>(
    transport: T,
    aid: &[u8],
) -> Result<OwnedEuiccChannel<T>, Error<T::Error>> {
    OwnedChannel::open_selected(ApduCardTransport::new(transport), aid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    enum FakeError {
        Failed,
    }

    impl fmt::Display for FakeError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("synthetic transport failure")
        }
    }

    impl std::error::Error for FakeError {}

    struct FakeApduTransport {
        replies: VecDeque<Result<Vec<u8>, FakeError>>,
        commands: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl ApduTransport for FakeApduTransport {
        type Error = FakeError;

        fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
            self.commands.lock().unwrap().push(command.to_vec());
            self.replies.pop_front().expect("test reply")
        }
    }

    #[test]
    fn adapter_preserves_command_and_response_with_status_word() {
        let fake = FakeApduTransport {
            replies: VecDeque::from([Ok(vec![0x01, 0x02, 0x90, 0x00])]),
            commands: Arc::new(Mutex::new(Vec::new())),
        };
        let mut transport = ApduCardTransport::new(fake);
        let mut response = vec![0xff];

        CardTransport::transmit(&mut transport, &[0x00, 0x84, 0x00, 0x00], &mut response).unwrap();

        assert_eq!(response, [0x01, 0x02, 0x90, 0x00]);
        assert_eq!(
            *transport.into_inner().commands.lock().unwrap(),
            [vec![0x00, 0x84, 0x00, 0x00]]
        );
    }

    #[test]
    fn adapter_clears_stale_response_after_transport_failure() {
        let fake = FakeApduTransport {
            replies: VecDeque::from([Err(FakeError::Failed)]),
            commands: Arc::new(Mutex::new(Vec::new())),
        };
        let mut transport = ApduCardTransport::new(fake);
        let mut response = vec![0xaa, 0xbb];

        assert!(matches!(
            CardTransport::transmit(&mut transport, &[0x00, 0x84, 0x00, 0x00], &mut response),
            Err(Error::Apdu(FakeError::Failed))
        ));
        assert!(response.is_empty());
    }

    #[test]
    fn owned_euicc_channel_selects_on_new_channel_and_closes_only_that_channel() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let fake = FakeApduTransport {
            replies: VecDeque::from([
                Ok(vec![0x01, 0x90, 0x00]), // MANAGE CHANNEL open
                Ok(vec![0x90, 0x00]),       // SELECT ISD-R
                Ok(vec![0x90, 0x00]),       // caller APDU
                Ok(vec![0x90, 0x00]),       // MANAGE CHANNEL close
            ]),
            commands: commands.clone(),
        };

        let aid = [0xa0, 0x00, 0x00, 0x05, 0x59, 0x10, 0x10];
        let mut channel = open_euicc_channel(fake, &aid).unwrap();
        assert_eq!(channel.channel(), 1);
        let mut response = Vec::new();
        CardTransport::transmit(&mut channel, &[0x80, 0xca, 0x00, 0x00], &mut response).unwrap();
        assert_eq!(response, [0x90, 0x00]);
        drop(channel);

        let commands = commands.lock().unwrap();
        assert_eq!(commands[0], [0x00, 0x70, 0x00, 0x00, 0x01]);
        assert_eq!(
            commands[1],
            [
                0x01, 0xa4, 0x04, 0x00, 0x07, 0xa0, 0x00, 0x00, 0x05, 0x59, 0x10, 0x10, 0x00
            ]
        );
        assert_eq!(commands[2], [0x81, 0xca, 0x00, 0x00]);
        assert_eq!(commands[3], [0x00, 0x70, 0x80, 0x01, 0x00]);
    }

    #[test]
    fn failed_select_closes_the_new_channel_and_preserves_primary_error() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let fake = FakeApduTransport {
            replies: VecDeque::from([
                Ok(vec![0x02, 0x90, 0x00]),
                Ok(vec![0x6a, 0x82]),
                Ok(vec![0x90, 0x00]),
            ]),
            commands: commands.clone(),
        };

        let result = open_euicc_channel(fake, &[0xa0, 0x00]);
        assert!(matches!(
            result,
            Err(Error::Protocol(CcidError::CardStatus(0x6a82)))
        ));
        assert_eq!(
            *commands.lock().unwrap(),
            [
                vec![0x00, 0x70, 0x00, 0x00, 0x01],
                vec![0x02, 0xa4, 0x04, 0x00, 0x02, 0xa0, 0x00, 0x00],
                vec![0x00, 0x70, 0x80, 0x02, 0x00],
            ]
        );
    }
}
