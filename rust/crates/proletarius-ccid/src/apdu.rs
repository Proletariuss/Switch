// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! ISO 7816 APDU command/response framing.

use alloc::vec::Vec;

use crate::{Error, Result};

pub const MAX_APDU_DATA: usize = 65_535;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Command<'a> {
    pub cla: u8,
    pub ins: u8,
    pub p1: u8,
    pub p2: u8,
    pub data: &'a [u8],
    /// `None` means no Le field. `Some(0)` encodes the maximum short/extended
    /// length according to the selected APDU form.
    pub le: Option<u32>,
}

impl Command<'_> {
    pub fn encoded_len(&self) -> Result<usize> {
        if self.data.len() > MAX_APDU_DATA {
            return Err(Error::LengthOverflow);
        }
        let extended = self.data.len() > 255 || self.le.is_some_and(|le| le > 256);
        if self.le.is_some_and(|le| le > 65_536) {
            return Err(Error::LengthOverflow);
        }
        let data_len = if self.data.is_empty() {
            0
        } else if extended {
            2 + self.data.len()
        } else {
            1 + self.data.len()
        };
        4_usize
            .checked_add(usize::from(extended))
            .and_then(|length| length.checked_add(data_len))
            .and_then(|length| {
                length.checked_add(match (extended, self.le) {
                    (_, None) => 0,
                    (true, Some(_)) => 2,
                    (false, Some(_)) => 1,
                })
            })
            .ok_or(Error::LengthOverflow)
    }

    /// Encode into caller-owned storage and return the exact APDU length.
    ///
    /// Validation happens before the first write, so an error never leaves a
    /// partially encoded command in `output`.
    pub fn encode_into(&self, output: &mut [u8]) -> Result<usize> {
        let length = self.encoded_len()?;
        if output.len() < length {
            return Err(Error::OutputTooSmall);
        }
        let extended = self.data.len() > 255 || self.le.is_some_and(|le| le > 256);
        let output = &mut output[..length];
        output[..4].copy_from_slice(&[self.cla, self.ins, self.p1, self.p2]);
        let mut offset = 4;
        if extended {
            output[offset] = 0;
            offset += 1;
            if !self.data.is_empty() {
                let length = u16::try_from(self.data.len()).map_err(|_| Error::LengthOverflow)?;
                output[offset..offset + 2].copy_from_slice(&length.to_be_bytes());
                offset += 2;
                output[offset..offset + self.data.len()].copy_from_slice(self.data);
                offset += self.data.len();
            }
            if let Some(le) = self.le {
                let encoded = if le == 0 || le == 65_536 {
                    0_u16
                } else {
                    u16::try_from(le).map_err(|_| Error::LengthOverflow)?
                };
                output[offset..offset + 2].copy_from_slice(&encoded.to_be_bytes());
                offset += 2;
            }
        } else {
            if !self.data.is_empty() {
                output[offset] = self.data.len() as u8;
                offset += 1;
                output[offset..offset + self.data.len()].copy_from_slice(self.data);
                offset += self.data.len();
            }
            if let Some(le) = self.le {
                output[offset] = if le == 0 || le == 256 { 0 } else { le as u8 };
                offset += 1;
            }
        }
        debug_assert_eq!(offset, length);
        Ok(length)
    }

    pub fn with_logical_channel(mut self, channel: u8) -> Result<Self> {
        self.cla = logical_channel_cla(self.cla, channel)?;
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Response<'a> {
    pub data: &'a [u8],
    pub status: u16,
}

impl<'a> Response<'a> {
    pub fn parse(input: &'a [u8]) -> Result<Self> {
        if input.len() < 2 {
            return Err(Error::Truncated);
        }
        let split = input.len() - 2;
        Ok(Self {
            data: &input[..split],
            status: u16::from_be_bytes([input[split], input[split + 1]]),
        })
    }

    #[must_use]
    pub const fn ok(self) -> bool {
        self.status == 0x9000
    }

    #[must_use]
    pub const fn has_more(self) -> bool {
        more_available((self.status >> 8) as u8)
    }

    #[must_use]
    pub const fn wrong_length(self) -> bool {
        (self.status >> 8) as u8 == 0x6c
    }

    /// Classify the status word the way the reference LPA reader does: the
    /// callers branch on `ProactiveCommand`, `ConditionsNotSatisfied` and
    /// `CardBusy` instead of re-deriving them from raw bytes.
    #[must_use]
    pub const fn class(self) -> StatusClass {
        classify(self.status)
    }

    pub fn result(self) -> Result<&'a [u8]> {
        if self.ok() {
            Ok(self.data)
        } else {
            Err(Error::CardStatus(self.status))
        }
    }
}

/// Status-word classes the card layer reacts to. `ProactiveCommand` (91xx)
/// means the eUICC has a proactive command pending, which the reference reader
/// answers by re-establishing the session rather than treating it as a plain
/// failure; `ConditionsNotSatisfied` (6985) and `CardBusy` (6881) are the two
/// retryable states it distinguishes from real errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusClass {
    Ok,
    MoreData,
    WrongLength,
    ProactiveCommand,
    ConditionsNotSatisfied,
    CardBusy,
    InvalidData,
    UnknownCommand,
    Failed,
}

/// Classify a raw status word without building a response view.
#[must_use]
pub const fn classify(status: u16) -> StatusClass {
    match status {
        0x9000 => StatusClass::Ok,
        0x6a80 => StatusClass::InvalidData,
        0x6a88 => StatusClass::UnknownCommand,
        0x6985 => StatusClass::ConditionsNotSatisfied,
        0x6881 => StatusClass::CardBusy,
        status if (status >> 8) as u8 == 0x61 || (status >> 8) as u8 == 0x9f => {
            StatusClass::MoreData
        }
        status if (status >> 8) as u8 == 0x6c => StatusClass::WrongLength,
        status if (status >> 8) as u8 == 0x91 => StatusClass::ProactiveCommand,
        _ => StatusClass::Failed,
    }
}

/// ISO 7816 logical-channel CLA encoding for channels 0..=19.
pub const fn logical_channel_cla(cla: u8, channel: u8) -> Result<u8> {
    crate::channel::channel_class(cla, channel)
}

/// Follow an "more data available" (61xx) answer with GET RESPONSE.
///
/// Every transport hands back `data || SW1 SW2`, which is exactly the layout
/// the reference LPA/USIM reader uses, so this is the only place Celmux reads
/// the remaining segments. GET RESPONSE keeps the logical channel of the
/// command that produced the pending data (ISO 7816-4 and the reference LPA
/// reader do the same): asking the basic channel for data a card prepared on a
/// logical channel makes it answer 6F00, which is how AKA on logical-channel
/// cards used to fail. A 6Cxx answer re-sends the same command with the length
/// the card asked for whenever the command already carries an Le byte.
pub fn transmit_follow<T>(
    transport: &mut T,
    command: &[u8],
    response: &mut Vec<u8>,
) -> core::result::Result<(), FollowError<T::Error>>
where
    T: crate::CardTransport,
{
    transport
        .transmit(command, response)
        .map_err(FollowError::Transport)?;
    if let Some(corrected) = corrected_length(command, response) {
        transport
            .transmit(&corrected, response)
            .map_err(FollowError::Transport)?;
    }
    if response.len() < 2 || !more_available(response[response.len() - 2]) {
        return Ok(());
    }
    let channel = command.first().copied().unwrap_or(0);
    let mut data = response[..response.len() - 2].to_vec();
    let mut status = [response[response.len() - 2], response[response.len() - 1]];
    let mut rounds = 0_usize;
    while more_available(status[0]) {
        rounds += 1;
        if rounds > MAX_GET_RESPONSE_ROUNDS {
            return Err(FollowError::Card(Error::Invalid(
                "too many GET RESPONSE rounds",
            )));
        }
        let le = if status[1] == 0 {
            256
        } else {
            u32::from(status[1])
        };
        let get = Command {
            cla: channel,
            ins: 0xc0,
            p1: 0,
            p2: 0,
            data: &[],
            le: Some(le),
        };
        let mut encoded = [0_u8; 8];
        let length = get.encode_into(&mut encoded).map_err(FollowError::Card)?;
        let mut segment = Vec::with_capacity(258);
        transport
            .transmit(&encoded[..length], &mut segment)
            .map_err(FollowError::Transport)?;
        if segment.len() < 2 {
            return Err(FollowError::Card(Error::Truncated));
        }
        data.extend_from_slice(&segment[..segment.len() - 2]);
        status = [segment[segment.len() - 2], segment[segment.len() - 1]];
    }
    response.clear();
    response.extend_from_slice(&data);
    response.extend_from_slice(&status);
    Ok(())
}

/// Rebuild `command` with the exact Le a 6Cxx answer asked for. Only short
/// case-2 and case-4 commands carry an Le byte that can be corrected in place;
/// everything else keeps the caller's explicit Le retry.
fn corrected_length(command: &[u8], response: &[u8]) -> Option<Vec<u8>> {
    if response.len() < 2 {
        return None;
    }
    let (sw1, sw2) = (response[response.len() - 2], response[response.len() - 1]);
    if sw1 != 0x6c {
        return None;
    }
    let le_index = match command.len() {
        5 => Some(4),
        length if length > 5 => {
            let body = usize::from(*command.get(4)?);
            (length == body.checked_add(6)?).then_some(length - 1)
        }
        _ => None,
    }?;
    let mut corrected = command.to_vec();
    *corrected.get_mut(le_index)? = sw2;
    Some(corrected)
}

/// Status bytes meaning "more data available", answered with GET RESPONSE.
///
/// 0x61xx is the standard form. 0x9Fxx is the proprietary variant a few USIM
/// and eUICC cards use for the same meaning; the reference readers treat both
/// alike, and accepting it is a superset that cannot break a conformant card.
pub const fn more_available(status_high: u8) -> bool {
    status_high == 0x61 || status_high == 0x9f
}

/// Failure while following a card response: the transport itself, or the
/// card answering with something unusable.
pub enum FollowError<E> {
    Transport(E),
    Card(Error),
}

/// Bound on GET RESPONSE rounds so a chatty card cannot loop forever.
const MAX_GET_RESPONSE_ROUNDS: usize = 16;
