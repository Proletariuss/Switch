// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! ISO 7816 logical channels over any raw APDU transport.
//!
//! PC/SC readers, the built-in USBFS CCID driver, QMI UIM slots and MBIM UICC
//! access all implement [`CardTransport`]. Logical-channel handling lives here
//! once so no transport grows its own MANAGE CHANNEL/SELECT dialect.

use alloc::vec::Vec;

use crate::apdu::{Command, Response};
use crate::{CardTransport, Error, Result};

/// Highest logical channel number a card may hand out.
pub const MAX_LOGICAL_CHANNEL: u8 = 19;

/// MANAGE CHANNEL: open a logical channel and return its number.
pub fn open_channel<T>(transport: &mut T) -> core::result::Result<u8, T::Error>
where
    T: CardTransport,
    T::Error: From<Error>,
{
    let mut response = Vec::with_capacity(258);
    transport.transmit(&[0x00, 0x70, 0x00, 0x00, 0x01], &mut response)?;
    let body = Response::parse(&response)?.result()?;
    let channel = *body
        .first()
        .ok_or(Error::Invalid("MANAGE CHANNEL returned no channel"))?;
    if !(1..=MAX_LOGICAL_CHANNEL).contains(&channel) {
        return Err(Error::Invalid("MANAGE CHANNEL returned an invalid channel").into());
    }
    Ok(channel)
}

/// MANAGE CHANNEL: close a logical channel.
///
/// Channel 0 is the basic channel: ISO 7816-4 keeps it open for the whole card
/// session and has no MANAGE CHANNEL close for it, so closing it is a no-op.
/// Sessions that fell back to the basic channel (cards that refuse SELECT on a
/// logical channel and are served by `open_for_aid`'s 1.0.2-style fallback)
/// end with channel 0 and must not fail their cleanup with an out-of-range
/// error after a successful operation.
pub fn close_channel<T>(transport: &mut T, channel: u8) -> core::result::Result<(), T::Error>
where
    T: CardTransport,
    T::Error: From<Error>,
{
    if channel == 0 {
        return Ok(());
    }
    if !(1..=MAX_LOGICAL_CHANNEL).contains(&channel) {
        return Err(Error::Invalid("logical channel is out of range").into());
    }
    let mut response = Vec::with_capacity(2);
    transport.transmit(&[0x00, 0x70, 0x80, channel, 0x00], &mut response)?;
    Response::parse(&response)?.result()?;
    Ok(())
}

/// Rewrite an APDU's class byte so it belongs to `channel`.
pub fn bind_command(command: &[u8], channel: u8) -> Result<Vec<u8>> {
    let Some((class, rest)) = command.split_first() else {
        return Err(Error::Invalid("APDU request is empty"));
    };
    let mut bound = Vec::with_capacity(command.len());
    bound.push(channel_class(*class, channel)?);
    bound.extend_from_slice(rest);
    Ok(bound)
}

/// Encode SELECT by AID on `channel`.
pub fn select_aid_command(channel: u8, aid: &[u8]) -> Result<Vec<u8>> {
    select_aid_command_with_le(channel, aid, Some(0))
}

fn select_aid_command_with_le(channel: u8, aid: &[u8], le: Option<u32>) -> Result<Vec<u8>> {
    if aid.is_empty() || aid.len() > u8::MAX as usize {
        return Err(Error::Invalid("logical-channel AID length is out of range"));
    }
    let select = Command {
        cla: channel_class(0, channel)?,
        ins: 0xa4,
        p1: 0x04,
        p2: 0,
        data: aid,
        // The reference reader always requests the FCI (Le = 0x00), which also
        // keeps SELECT a case-2 command on cards that only answer those.
        le,
    };
    let mut encoded = [0_u8; 260];
    let length = select.encode_into(&mut encoded)?;
    Ok(encoded[..length].to_vec())
}

/// SELECT by AID on an open logical channel, returning the card's FCI.
pub fn select_aid<T>(
    transport: &mut T,
    channel: u8,
    aid: &[u8],
) -> core::result::Result<Vec<u8>, T::Error>
where
    T: CardTransport,
    T::Error: From<Error>,
{
    select_aid_with_le(transport, channel, aid, Some(0))
}

/// SELECT by AID with an explicit Le.
///
/// The reference reader always requests the FCI (Le = 0x00), but some
/// programmable cards only answer the case-3 form 1.0.2 used (`le = None`).
/// Callers that can recognise a command-format status retry with `None`.
pub fn select_aid_with_le<T>(
    transport: &mut T,
    channel: u8,
    aid: &[u8],
    le: Option<u32>,
) -> core::result::Result<Vec<u8>, T::Error>
where
    T: CardTransport,
    T::Error: From<Error>,
{
    let command = select_aid_command_with_le(channel, aid, le).map_err(T::Error::from)?;
    let mut response = Vec::with_capacity(258);
    transport.transmit(&command, &mut response)?;
    let parsed = Response::parse(&response)?;
    // Cards may answer SELECT with 61xx and expect GET RESPONSE next; that is
    // still a successful selection.
    if !(parsed.ok() || parsed.has_more()) {
        return Err(Error::CardStatus(parsed.status).into());
    }
    Ok(parsed.data.to_vec())
}

/// Open a logical channel and select `aid`, closing it again on failure.
///
/// The reference LPA reader cleans the logical channels a previous session may
/// have left open and, when the card refuses MANAGE CHANNEL outright, falls
/// back to selecting the AID on the basic channel instead of failing. Cards
/// that hand out a logical channel but refuse the AID on it (several
/// programmable eUICCs do) are served the same way: the basic channel is the
/// path 1.0.2 used for every card and still selects the application.
pub fn open_for_aid<T>(transport: &mut T, aid: &[u8]) -> core::result::Result<u8, T::Error>
where
    T: CardTransport,
    T::Error: From<Error>,
{
    close_stale_channels(transport);
    let channel = match open_channel(transport) {
        Ok(channel) => channel,
        Err(_) => {
            select_aid(transport, 0, aid)?;
            return Ok(0);
        }
    };
    match select_aid(transport, channel, aid) {
        Ok(_) => Ok(channel),
        Err(primary) => {
            close_channel(transport, channel).ok();
            match select_aid(transport, 0, aid) {
                Ok(_) => Ok(0),
                Err(_) => Err(primary),
            }
        }
    }
}

/// Best-effort close of the low logical channels a previous session may have
/// left open. Failures are ignored: cards that never opened one answer with an
/// error the same way the reference reader ignores it.
pub fn close_stale_channels<T>(transport: &mut T)
where
    T: CardTransport,
    T::Error: From<Error>,
{
    for channel in 1..=3 {
        close_channel(transport, channel).ok();
    }
}

/// Open one channel and select the first AID the card accepts.
pub fn open_first<T>(
    transport: &mut T,
    aids: &[&[u8]],
) -> core::result::Result<(u8, Vec<u8>), T::Error>
where
    T: CardTransport,
    T::Error: From<Error>,
{
    let channel = open_channel(transport)?;
    let mut last_error = None;
    for aid in aids {
        match select_aid(transport, channel, aid) {
            Ok(_) => return Ok((channel, aid.to_vec())),
            Err(error) => last_error = Some(error),
        }
    }
    if close_channel(transport, channel).is_err() {
        return Err(Error::ChannelCleanupUnconfirmed.into());
    }
    Err(last_error.unwrap_or_else(|| Error::Invalid("no supported eUICC AID").into()))
}

/// One logical channel owning its transport, closed on drop.
///
/// This is the only channel wrapper Celmux needs: reader, QMI and MBIM
/// sessions all hand a raw [`CardTransport`] to [`OwnedChannel::open`].
pub struct OwnedChannel<T: CardTransport>
where
    T::Error: From<Error>,
{
    transport: T,
    channel: u8,
    closed: bool,
    cleanup_unconfirmed: bool,
}

impl<T: CardTransport> OwnedChannel<T>
where
    T::Error: From<Error>,
{
    /// Open a channel and select `aid` on it.
    pub fn open(transport: T, aid: &[u8]) -> core::result::Result<Self, T::Error> {
        let mut transport = transport;
        let channel = open_for_aid(&mut transport, aid)?;
        Ok(Self {
            transport,
            channel,
            closed: false,
            cleanup_unconfirmed: false,
        })
    }

    /// Open a newly allocated logical channel and select `aid` without
    /// probing or closing channels that may belong to another card client.
    ///
    /// This matches Switch's existing PC/SC LPA behavior: channel ownership is
    /// acquired with MANAGE CHANNEL, a failed SELECT closes only that channel,
    /// and unrelated channel numbers are left untouched.
    pub fn open_selected(transport: T, aid: &[u8]) -> core::result::Result<Self, T::Error> {
        let mut transport = transport;
        let channel = open_channel(&mut transport)?;
        if let Err(error) = select_aid(&mut transport, channel, aid) {
            // A failed close leaves channel ownership unknown. Never turn that
            // outcome back into a plain AID miss or probe another channel.
            if close_channel(&mut transport, channel).is_err() {
                return Err(Error::ChannelCleanupUnconfirmed.into());
            }
            return Err(error);
        }
        Ok(Self {
            transport,
            channel,
            closed: false,
            cleanup_unconfirmed: false,
        })
    }

    /// Open a channel and select the first AID the card accepts.
    pub fn open_first(
        transport: T,
        aids: &[&[u8]],
    ) -> core::result::Result<(Self, Vec<u8>), T::Error> {
        let mut transport = transport;
        let (channel, aid) = open_first(&mut transport, aids)?;
        Ok((
            Self {
                transport,
                channel,
                closed: false,
                cleanup_unconfirmed: false,
            },
            aid,
        ))
    }

    #[must_use]
    pub const fn channel(&self) -> u8 {
        self.channel
    }

    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Close the logical channel, keeping the transport for reuse.
    pub fn close(&mut self) -> core::result::Result<(), T::Error> {
        if self.cleanup_unconfirmed {
            // A lost response is not permission to send another close APDU.
            // Its channel number could already have been reused by the card.
            return Err(Error::ChannelCleanupUnconfirmed.into());
        }
        if self.closed {
            return Ok(());
        }
        match close_channel(&mut self.transport, self.channel) {
            Ok(()) => {
                self.closed = true;
                Ok(())
            }
            Err(error) => {
                self.cleanup_unconfirmed = true;
                Err(error)
            }
        }
    }
}

impl<T: CardTransport> CardTransport for OwnedChannel<T>
where
    T::Error: From<Error>,
{
    type Error = T::Error;

    fn transmit(
        &mut self,
        command: &[u8],
        response: &mut Vec<u8>,
    ) -> core::result::Result<(), Self::Error> {
        if self.cleanup_unconfirmed {
            return Err(Error::ChannelCleanupUnconfirmed.into());
        }
        if self.closed {
            return Err(Error::Invalid("logical channel is closed").into());
        }
        let bound = bind_command(command, self.channel).map_err(T::Error::from)?;
        self.transport.transmit(&bound, response)
    }
}

impl<T: CardTransport> Drop for OwnedChannel<T>
where
    T::Error: From<Error>,
{
    fn drop(&mut self) {
        self.close().ok();
    }
}

/// Class byte for a logical channel, per ISO 7816-4 (channels 4..19 use the
/// 0x40 encoding).
pub const fn channel_class(class: u8, channel: u8) -> Result<u8> {
    if channel == 0 {
        return Ok(class);
    }
    if channel < 4 {
        return Ok((class & 0x9c) | channel);
    }
    if channel <= MAX_LOGICAL_CHANNEL {
        return Ok((class & 0xb0) | 0x40 | (channel - 4));
    }
    Err(Error::Invalid("logical channel is out of range"))
}

#[cfg(test)]
mod open_selected_cleanup_tests {
    use super::*;
    use alloc::{collections::VecDeque, rc::Rc, vec};
    use core::cell::RefCell;

    #[derive(Debug, Eq, PartialEq)]
    enum FakeError {
        Io,
        Protocol(Error),
    }

    impl From<Error> for FakeError {
        fn from(error: Error) -> Self {
            Self::Protocol(error)
        }
    }

    struct ScriptedCard {
        replies: VecDeque<core::result::Result<Vec<u8>, FakeError>>,
        commands: Rc<RefCell<Vec<Vec<u8>>>>,
    }

    impl CardTransport for ScriptedCard {
        type Error = FakeError;

        fn transmit(
            &mut self,
            command: &[u8],
            response: &mut Vec<u8>,
        ) -> core::result::Result<(), Self::Error> {
            self.commands.borrow_mut().push(command.to_vec());
            *response = self.replies.pop_front().expect("scripted APDU response")?;
            Ok(())
        }
    }

    #[test]
    fn failed_close_stays_unconfirmed_and_drop_never_replays_apdu() {
        for close in [Err(FakeError::Io), Ok(vec![0x6a, 0x81])] {
            let commands = Rc::new(RefCell::new(Vec::new()));
            let card = ScriptedCard {
                replies: VecDeque::from([Ok(vec![0x01, 0x90, 0x00]), Ok(vec![0x90, 0x00]), close]),
                commands: commands.clone(),
            };
            let mut channel = OwnedChannel::open_selected(card, &[0xa0, 0x00]).unwrap();
            assert!(channel.close().is_err());
            assert!(
                !channel.is_closed(),
                "failed close is not physical confirmation"
            );
            assert_eq!(
                channel.close(),
                Err(FakeError::Protocol(Error::ChannelCleanupUnconfirmed))
            );
            let mut response = Vec::new();
            assert_eq!(
                channel.transmit(&[0x00, 0x84, 0x00, 0x00], &mut response),
                Err(FakeError::Protocol(Error::ChannelCleanupUnconfirmed))
            );
            drop(channel);
            assert_eq!(commands.borrow().len(), 3, "no APDU after uncertain close");
        }
    }

    #[test]
    fn confirmed_close_is_idempotent_without_another_apdu() {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let card = ScriptedCard {
            replies: VecDeque::from([
                Ok(vec![0x01, 0x90, 0x00]),
                Ok(vec![0x90, 0x00]),
                Ok(vec![0x90, 0x00]),
            ]),
            commands: commands.clone(),
        };
        let mut channel = OwnedChannel::open_selected(card, &[0xa0, 0x00]).unwrap();
        channel.close().unwrap();
        assert!(channel.is_closed());
        channel.close().unwrap();
        drop(channel);
        assert_eq!(commands.borrow().len(), 3);
    }

    #[test]
    fn failed_select_close_failure_is_cleanup_unknown() {
        for close in [Err(FakeError::Io), Ok(vec![0x6a, 0x81])] {
            let commands = Rc::new(RefCell::new(Vec::new()));
            let card = ScriptedCard {
                replies: VecDeque::from([Ok(vec![0x01, 0x90, 0x00]), Ok(vec![0x6a, 0x82]), close]),
                commands: commands.clone(),
            };
            assert!(matches!(
                OwnedChannel::open_selected(card, &[0xa0, 0x00]),
                Err(FakeError::Protocol(Error::ChannelCleanupUnconfirmed))
            ));
            let commands = commands.borrow();
            assert_eq!(commands.len(), 3);
            assert_eq!(commands[2], [0x00, 0x70, 0x80, 0x01, 0x00]);
        }
    }

    #[test]
    fn failed_select_with_confirmed_close_keeps_aid_miss() {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let card = ScriptedCard {
            replies: VecDeque::from([
                Ok(vec![0x01, 0x90, 0x00]),
                Ok(vec![0x6a, 0x82]),
                Ok(vec![0x90, 0x00]),
            ]),
            commands: commands.clone(),
        };
        assert!(matches!(
            OwnedChannel::open_selected(card, &[0xa0, 0x00]),
            Err(FakeError::Protocol(Error::CardStatus(0x6a82)))
        ));
        assert_eq!(commands.borrow().len(), 3);
    }
}
