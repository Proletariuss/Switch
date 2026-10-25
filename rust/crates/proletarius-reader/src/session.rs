//! Exclusive APDU ownership for the eventual native reader runtime.
//! Discovery must never construct this type while Go owns a reader.
use std::ffi::CString;
use std::fmt;

use pcsc::{Card, Context, Disposition, Protocols, Scope, ShareMode, Transaction};
use proletarius_sim::ApduTransport;

#[derive(Debug)]
pub enum SessionError {
    InvalidReaderName,
    InvalidCommandLength,
    Closed,
    Pcsc(pcsc::Error),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReaderName => f.write_str("invalid PC/SC reader name"),
            Self::InvalidCommandLength => f.write_str("unsupported APDU length"),
            Self::Closed => f.write_str("PC/SC session is closed"),
            Self::Pcsc(error) => write!(f, "PC/SC operation failed: {error}"),
        }
    }
}
impl std::error::Error for SessionError {}

impl SessionError {
    pub fn is_unresponsive(&self) -> bool {
        matches!(self, Self::Pcsc(pcsc::Error::UnresponsiveCard))
    }

    pub fn is_card_absent(&self) -> bool {
        matches!(
            self,
            Self::Pcsc(pcsc::Error::NoSmartcard | pcsc::Error::RemovedCard)
        )
    }
    pub fn is_reader_unavailable(&self) -> bool {
        matches!(self, Self::Pcsc(pcsc::Error::NoReadersAvailable))
    }
    /// Whether PC/SC rejected the transaction because another operation owns
    /// the card or requested a reset before the session could begin.
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Pcsc(pcsc::Error::SharingViolation | pcsc::Error::ResetCard)
        )
    }
}

pub struct PcscSession {
    card: Option<Card>,
    close_disposition: Disposition,
}

pub struct PcscTransaction<'session>(Transaction<'session>);

impl PcscTransaction<'_> {
    /// Confirm the OS transaction ended instead of treating Drop as an ACK.
    /// On failure the caller must close/quarantine the owning card session.
    pub fn end(self) -> Result<(), SessionError> {
        self.0
            .end(Disposition::LeaveCard)
            .map_err(|(_transaction, error)| SessionError::Pcsc(error))
    }
}

impl PcscSession {
    /// Enumeration only: selecting an empty reader must not power a card or
    /// issue an APDU.
    pub fn reader_names() -> Result<Vec<String>, SessionError> {
        let context = Context::establish(Scope::System).map_err(SessionError::Pcsc)?;
        context
            .list_readers_owned()
            .map(|names| {
                names
                    .into_iter()
                    .map(|name| name.to_string_lossy().into_owned())
                    .collect()
            })
            .map_err(SessionError::Pcsc)
    }

    /// Explicit recovery is deliberately separate from transmit. Callers may
    /// revive a failed open, but must never replay an APDU after this action.
    pub fn connect_for_reset(reader: &str, power_cycle: bool) -> Result<Self, SessionError> {
        if reader.trim().is_empty() {
            return Err(SessionError::InvalidReaderName);
        }
        let name = CString::new(reader).map_err(|_| SessionError::InvalidReaderName)?;
        let context = Context::establish(Scope::System).map_err(SessionError::Pcsc)?;
        let card = context
            .connect(&name, ShareMode::Exclusive, Protocols::ANY)
            .map_err(SessionError::Pcsc)?;
        Ok(Self {
            card: Some(card),
            close_disposition: if power_cycle {
                Disposition::UnpowerCard
            } else {
                Disposition::ResetCard
            },
        })
    }

    /// Reader discovery and eSIM status refresh may legitimately keep another
    /// shared PC/SC handle alive.  Match macOS' normal smart-card ownership
    /// model here, then use `transaction()` to serialize the complete AKA APDU
    /// sequence without requiring every observer to disconnect first.
    pub fn connect(reader: &str) -> Result<Self, SessionError> {
        if reader.trim().is_empty() {
            return Err(SessionError::InvalidReaderName);
        }
        let name = CString::new(reader).map_err(|_| SessionError::InvalidReaderName)?;
        let context = Context::establish(Scope::System).map_err(SessionError::Pcsc)?;
        let card = context
            .connect(&name, ShareMode::Shared, Protocols::ANY)
            .map_err(SessionError::Pcsc)?;
        Ok(Self {
            card: Some(card),
            close_disposition: Disposition::LeaveCard,
        })
    }

    pub fn transaction(&mut self) -> Result<PcscTransaction<'_>, SessionError> {
        self.card
            .as_mut()
            .ok_or(SessionError::Closed)?
            .transaction()
            .map(PcscTransaction)
            .map_err(SessionError::Pcsc)
    }

    /// Explicit close reports failures and retains the handle for retry.
    pub fn close(&mut self) -> Result<(), SessionError> {
        if let Some(card) = self.card.take()
            && let Err((card, error)) = card.disconnect(self.close_disposition)
        {
            self.card = Some(card);
            return Err(SessionError::Pcsc(error));
        }
        Ok(())
    }
}

impl ApduTransport for PcscSession {
    type Error = SessionError;

    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
        if !(4..=65544).contains(&command.len()) {
            return Err(SessionError::InvalidCommandLength);
        }
        let card = self.card.as_ref().ok_or(SessionError::Closed)?;
        let mut response = vec![0u8; pcsc::MAX_BUFFER_SIZE_EXTENDED];
        card.transmit(command, &mut response)
            .map(<[u8]>::to_vec)
            .map_err(SessionError::Pcsc)
    }
}

impl ApduTransport for PcscTransaction<'_> {
    type Error = SessionError;

    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
        if !(4..=65544).contains(&command.len()) {
            return Err(SessionError::InvalidCommandLength);
        }
        let mut response = vec![0u8; pcsc::MAX_BUFFER_SIZE_EXTENDED];
        self.0
            .transmit(command, &mut response)
            .map(<[u8]>::to_vec)
            .map_err(SessionError::Pcsc)
    }
}

impl Drop for PcscSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_session_never_sends_commands() {
        let mut session = PcscSession {
            card: None,
            close_disposition: Disposition::LeaveCard,
        };
        assert!(matches!(
            session.transmit(&[0, 0xa4, 0, 0]),
            Err(SessionError::Closed)
        ));
        assert!(matches!(
            session.transmit(&[]),
            Err(SessionError::InvalidCommandLength)
        ));
        session.close().unwrap();
        session.close().unwrap();
    }

    #[test]
    fn invalid_names_fail_before_pcsc_access() {
        for name in ["", "  ", "reader\0other"] {
            assert!(matches!(
                PcscSession::connect(name),
                Err(SessionError::InvalidReaderName)
            ));
        }
    }

    #[test]
    fn no_reader_classification_preserves_unavailable_semantics() {
        assert!(SessionError::Pcsc(pcsc::Error::NoReadersAvailable).is_reader_unavailable());
        assert!(!SessionError::Pcsc(pcsc::Error::NoSmartcard).is_reader_unavailable());
        assert!(!SessionError::Closed.is_reader_unavailable());
    }
}
