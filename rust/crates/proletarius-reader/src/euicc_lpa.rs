//! Compose the reusable eUICC and SGP.22 LPA crates over a reader APDU stream.
//!
//! This is deliberately a transport constructor rather than a device opener:
//! callers must hold the reader's cross-process lease and operation lock for
//! the complete lifetime of the returned client. That lets PC/SC, BLE, and
//! module-backed APDU transports share one protocol path without duplicating
//! Celmux's MIT-licensed SGP.22 state machine or weakening Switch's ownership
//! rules.

use std::fmt;

use proletarius_euicc::Euicc;
use proletarius_lpa::{Client, Es9Transport};
use proletarius_sim::ApduTransport;

use crate::euicc_transport::{
    Error as ReaderTransportError, OwnedEuiccChannel, open_euicc_channel,
};

/// Match the current Go standalone reader's custom-transport STORE DATA chunk
/// size. Module/QMI sessions use a different 120-byte setting.
pub const READER_LPA_MSS: usize = 240;

pub type ReaderLpaClient<T, N> = Client<Euicc<OwnedEuiccChannel<T>>, N>;

#[derive(Debug)]
pub enum ClientOpenError<E> {
    Channel(ReaderTransportError<E>),
    Euicc(proletarius_euicc::Error<ReaderTransportError<E>>),
}

impl<E: fmt::Display> fmt::Display for ClientOpenError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Channel(error) => write!(formatter, "open eUICC logical channel: {error}"),
            Self::Euicc(error) => write!(formatter, "initialize eUICC LPA: {error}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for ClientOpenError<E> {}

/// Open one newly allocated eUICC logical channel and compose it with an ES9+
/// transport. Existing card channels are left untouched; dropping the client
/// closes only the channel allocated here.
pub fn open_lpa_client<T, N>(
    transport: T,
    aid: &[u8],
    mss: usize,
    network: N,
) -> Result<ReaderLpaClient<T, N>, ClientOpenError<T::Error>>
where
    T: ApduTransport,
    T::Error: fmt::Display,
    N: Es9Transport,
{
    let channel = open_euicc_channel(transport, aid).map_err(ClientOpenError::Channel)?;
    let channel_number = channel.channel();
    let euicc = Euicc::new(channel, channel_number, mss).map_err(ClientOpenError::Euicc)?;
    Ok(Client::new(euicc, network))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, convert::Infallible};

    use proletarius_lpa::{Es9Transport, HttpError};

    struct FakeApdu(VecDeque<Result<Vec<u8>, Infallible>>);

    impl ApduTransport for FakeApdu {
        type Error = Infallible;

        fn transmit(&mut self, _command: &[u8]) -> Result<Vec<u8>, Self::Error> {
            self.0
                .pop_front()
                .expect("fixture APDU reply")
                .map_err(|never| match never {})
        }
    }

    #[derive(Default)]
    struct FakeEs9;

    impl Es9Transport for FakeEs9 {
        type Error = HttpError;

        fn initiate(
            &self,
            _activation: &proletarius_lpa::ActivationCode,
            _challenge: &[u8],
            _euicc_info1: &[u8],
            _timeout: std::time::Duration,
        ) -> Result<proletarius_lpa::InitiateResponse, Self::Error> {
            Err(HttpError::Invalid("unexpected fixture call"))
        }

        fn authenticate_client(
            &self,
            _activation: &proletarius_lpa::ActivationCode,
            _server_transaction_id: &str,
            _authenticate_server_response: &[u8],
            _timeout: std::time::Duration,
        ) -> Result<proletarius_lpa::AuthenticateResponse, Self::Error> {
            Err(HttpError::Invalid("unexpected fixture call"))
        }

        fn get_bound_profile_package(
            &self,
            _activation: &proletarius_lpa::ActivationCode,
            _server_transaction_id: &str,
            _prepare_download_response: &[u8],
            _timeout: std::time::Duration,
        ) -> Result<proletarius_lpa::BoundProfilePackage, Self::Error> {
            Err(HttpError::Invalid("unexpected fixture call"))
        }

        fn handle_notification(
            &self,
            _address: &str,
            _notification: &[u8],
            _timeout: std::time::Duration,
        ) -> Result<(), Self::Error> {
            Err(HttpError::Invalid("unexpected fixture call"))
        }

        fn cancel_session(
            &self,
            _activation: &proletarius_lpa::ActivationCode,
            _server_transaction_id: &str,
            _cancel_response: &[u8],
            _timeout: std::time::Duration,
        ) -> Result<(), Self::Error> {
            Err(HttpError::Invalid("unexpected fixture call"))
        }
    }

    #[test]
    fn client_composes_owned_channel_euicc_and_es9_transport() {
        let apdu = FakeApdu(VecDeque::from([
            Ok(vec![0x01, 0x90, 0x00]), // MANAGE CHANNEL open
            Ok(vec![0x90, 0x00]),       // SELECT requested eUICC AID
            Ok(vec![0x90, 0x00]),       // RAII MANAGE CHANNEL close
        ]));
        let aid = [0xa0, 0x00, 0x00, 0x05, 0x59, 0x10, 0x10];

        let mut client = open_lpa_client(apdu, &aid, READER_LPA_MSS, FakeEs9).unwrap();

        assert_eq!(client.card_mut().logical_channel(), 1);
        assert_eq!(client.card_mut().mss(), READER_LPA_MSS);
        drop(client); // OwnedEuiccChannel closes channel 1 by RAII.
    }

    #[test]
    fn caller_can_preserve_transport_specific_mss() {
        let apdu = FakeApdu(VecDeque::from([
            Ok(vec![0x01, 0x90, 0x00]),
            Ok(vec![0x90, 0x00]),
            Ok(vec![0x90, 0x00]),
        ]));

        let mut client = open_lpa_client(apdu, &[0xa0, 0x00], 120, FakeEs9).unwrap();

        assert_eq!(client.card_mut().mss(), 120);
    }

    #[test]
    fn select_failure_is_reported_before_an_lpa_client_is_exposed() {
        let apdu = FakeApdu(VecDeque::from([
            Ok(vec![0x02, 0x90, 0x00]),
            Ok(vec![0x6a, 0x82]),
            Ok(vec![0x90, 0x00]), // close the channel allocated by this attempt
        ]));

        let result = open_lpa_client(apdu, &[0xa0, 0x00], READER_LPA_MSS, FakeEs9);

        assert!(matches!(
            result,
            Err(ClientOpenError::Channel(ReaderTransportError::Protocol(
                proletarius_ccid::Error::CardStatus(0x6a82)
            )))
        ));
    }
}
