// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! Small, transport-independent smart-card protocol primitives.
//!
//! The crate deliberately stops at byte ownership boundaries.  Linux USBFS,
//! QMI UIM, modem AT bridges and HTTP LPA code can all use the same APDU and
//! BER-TLV codecs without sharing a card-session lock or a background worker.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt;

pub mod apdu;
pub mod bertlv;
pub mod ccid;
pub mod channel;
pub mod t0;
pub mod t1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Truncated,
    Invalid(&'static str),
    LengthOverflow,
    OutputTooSmall,
    Unsupported(&'static str),
    CardStatus(u16),
    ChannelCleanupUnconfirmed,
}

impl Error {
    /// Typed view of a card status word, so callers branch on the class the
    /// reference reader distinguishes instead of parsing the message text.
    #[must_use]
    pub const fn status_class(&self) -> Option<apdu::StatusClass> {
        match self {
            Self::CardStatus(status) => Some(apdu::classify(*status)),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("smart-card frame is truncated"),
            Self::Invalid(reason) => write!(formatter, "invalid smart-card frame: {reason}"),
            Self::LengthOverflow => formatter.write_str("smart-card length exceeds the limit"),
            Self::OutputTooSmall => formatter.write_str("smart-card output buffer is too small"),
            Self::Unsupported(reason) => {
                write!(formatter, "unsupported smart-card feature: {reason}")
            }
            Self::CardStatus(status) => write!(formatter, "smart-card status word {status:04X}"),
            Self::ChannelCleanupUnconfirmed => {
                formatter.write_str("logical channel cleanup unconfirmed")
            }
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// A backend capable of exchanging one complete ISO 7816 APDU.
///
/// Implementations replace `response` with the card response, including its
/// two-byte status word. Keeping the buffers caller-owned lets USBFS and QMI
/// transports reuse their allocations.
pub trait CardTransport {
    type Error;

    fn transmit(
        &mut self,
        command: &[u8],
        response: &mut Vec<u8>,
    ) -> core::result::Result<(), Self::Error>;
}

/// Append a bounded byte range without allowing a caller-controlled integer
/// to wrap a `usize` calculation.
pub(crate) fn checked_extend(output: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    output
        .len()
        .checked_add(bytes.len())
        .ok_or(Error::LengthOverflow)?;
    output.extend_from_slice(bytes);
    Ok(())
}
