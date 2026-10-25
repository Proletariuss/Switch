//! ISO/IEC 7816-3 T=1 block framing and the T=1 link state machine.
//!
//! The state machine follows libccid's `t1_transceive`: sequence numbers,
//! retransmission (rule 7.x), the RESYNC procedure (rule 6.4), IFS and WTX
//! handling, and a terminal DEAD state for a link that cannot be recovered.

use alloc::vec::Vec;

use crate::{Error, Result};

const MAX_INFORMATION: usize = 254;
/// Retransmissions of one block before the link runs RESYNC (rule 7.x).
const RETRIES: u8 = 3;
/// RESYNC attempts before the link is DEAD (rule 6.4).
const MAX_RESYNCS: u8 = 3;
/// Frames one exchange may consume, and bytes it may collect.
///
/// Every accepted frame resets the retransmission budget, which is right while
/// the card is making progress and unbounded when it is not: a card that keeps
/// answering "more follows" - with information or without - leaves the loop
/// with nothing that would ever stop it. The reader session it holds is the
/// only session that reader has, so the request behind it would wait until the
/// process restarted. An exchange therefore carries a budget of its own, which
/// no progress resets.
const MAX_EXCHANGE_FRAMES: usize = 1024;
const MAX_EXCHANGE_BYTES: usize = 1 << 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edc {
    Lrc,
    Crc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockKind {
    Information {
        sequence: u8,
        more: bool,
    },
    ReceiveReady {
        sequence: u8,
        error: Option<ReceiveError>,
    },
    Supervisory {
        response: bool,
        kind: SupervisoryKind,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveError {
    Edc,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisoryKind {
    Resync,
    Ifs,
    Abort,
    WaitingTimeExtension,
}

/// Why a received frame could not be turned into a block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// Shorter than NAD, PCB, LEN and the checksum.
    Truncated,
    /// Length or protocol control bytes are unusable.
    Malformed,
    /// The error detection code does not match.
    Edc,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    pub nad: u8,
    pub kind: BlockKind,
    pub information: Vec<u8>,
}

impl Block {
    #[must_use]
    pub fn information(nad: u8, sequence: u8, more: bool, information: &[u8]) -> Self {
        Self {
            nad,
            kind: BlockKind::Information {
                sequence: sequence & 1,
                more,
            },
            information: information.to_vec(),
        }
    }

    #[must_use]
    pub fn receive_ready(nad: u8, sequence: u8, error: Option<ReceiveError>) -> Self {
        Self {
            nad,
            kind: BlockKind::ReceiveReady {
                sequence: sequence & 1,
                error,
            },
            information: Vec::new(),
        }
    }

    #[must_use]
    pub fn supervisory(nad: u8, response: bool, kind: SupervisoryKind, information: &[u8]) -> Self {
        Self {
            nad,
            kind: BlockKind::Supervisory { response, kind },
            information: information.to_vec(),
        }
    }

    pub fn encode(&self, edc: Edc) -> Result<Vec<u8>> {
        if self.information.len() > MAX_INFORMATION {
            return Err(Error::LengthOverflow);
        }
        let pcb = self.pcb()?;
        let mut output = Vec::with_capacity(3 + self.information.len() + edc.len());
        output.extend_from_slice(&[self.nad, pcb, self.information.len() as u8]);
        output.extend_from_slice(&self.information);
        append_edc(&mut output, edc);
        Ok(output)
    }

    /// Decode one frame, keeping the failure class so the state machine can
    /// tell an EDC error (rule 7.2 retransmission) from unusable garbage.
    pub fn try_decode(input: &[u8], edc: Edc) -> core::result::Result<Self, DecodeError> {
        if input.len() < 3 + edc.len() {
            return Err(DecodeError::Truncated);
        }
        let information_len = usize::from(input[2]);
        let total = 3usize
            .checked_add(information_len)
            .and_then(|value| value.checked_add(edc.len()))
            .ok_or(DecodeError::Malformed)?;
        if total != input.len() {
            return Err(DecodeError::Malformed);
        }
        if !verify_edc(input, edc) {
            return Err(DecodeError::Edc);
        }
        let pcb = input[1];
        let kind = if pcb & 0x80 == 0 {
            BlockKind::Information {
                sequence: (pcb >> 6) & 1,
                more: pcb & 0x20 != 0,
            }
        } else if pcb & 0x40 == 0 {
            BlockKind::ReceiveReady {
                sequence: (pcb >> 4) & 1,
                error: match pcb & 0x0f {
                    0 => None,
                    1 => Some(ReceiveError::Edc),
                    2 => Some(ReceiveError::Other),
                    _ => return Err(DecodeError::Malformed),
                },
            }
        } else {
            let response = pcb & 0x20 != 0;
            let kind = match pcb & 0x0f {
                0 => SupervisoryKind::Resync,
                1 => SupervisoryKind::Ifs,
                2 => SupervisoryKind::Abort,
                3 => SupervisoryKind::WaitingTimeExtension,
                _ => return Err(DecodeError::Malformed),
            };
            BlockKind::Supervisory { response, kind }
        };
        Ok(Self {
            nad: input[0],
            kind,
            information: input[3..3 + information_len].to_vec(),
        })
    }

    pub fn decode(input: &[u8], edc: Edc) -> Result<Self> {
        Self::try_decode(input, edc).map_err(|error| match error {
            DecodeError::Truncated => Error::Truncated,
            DecodeError::Malformed => Error::Invalid("T=1 block"),
            DecodeError::Edc => Error::Invalid("T=1 block EDC"),
        })
    }

    fn pcb(&self) -> Result<u8> {
        match self.kind {
            BlockKind::Information { sequence, more } => {
                Ok((sequence & 1) << 6 | if more { 0x20 } else { 0 })
            }
            BlockKind::ReceiveReady { sequence, error } => Ok(0x80
                | ((sequence & 1) << 4)
                | match error {
                    None => 0,
                    Some(ReceiveError::Edc) => 1,
                    Some(ReceiveError::Other) => 2,
                }),
            BlockKind::Supervisory { response, kind } => Ok(0xc0
                | if response { 0x20 } else { 0 }
                | match kind {
                    SupervisoryKind::Resync => 0,
                    SupervisoryKind::Ifs => 1,
                    SupervisoryKind::Abort => 2,
                    SupervisoryKind::WaitingTimeExtension => 3,
                }),
        }
    }
}

impl Edc {
    const fn len(self) -> usize {
        match self {
            Self::Lrc => 1,
            Self::Crc => 2,
        }
    }
}

fn append_edc(output: &mut Vec<u8>, edc: Edc) {
    match edc {
        Edc::Lrc => output.push(output.iter().fold(0, |sum, byte| sum ^ byte)),
        Edc::Crc => output.extend_from_slice(&crc(&output[..]).to_le_bytes()),
    }
}

fn verify_edc(input: &[u8], edc: Edc) -> bool {
    let payload_len = input.len() - edc.len();
    match edc {
        Edc::Lrc => {
            input[payload_len] == input[..payload_len].iter().fold(0, |sum, byte| sum ^ byte)
        }
        Edc::Crc => {
            u16::from_le_bytes([input[payload_len], input[payload_len + 1]])
                == crc(&input[..payload_len])
        }
    }
}

fn crc(bytes: &[u8]) -> u16 {
    let mut value = 0xffff;
    for byte in bytes {
        value ^= u16::from(*byte);
        for _ in 0..8 {
            value = if value & 1 != 0 {
                (value >> 1) ^ 0x8408
            } else {
                value >> 1
            };
        }
    }
    !value
}

/// T=1 link state (ISO 7816-3 rule 6.x).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Sending,
    Receiving,
    Resync,
    Dead,
}

/// Failure of one block exchange.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExchangeError<E> {
    /// The reader or card signalled a parity error; retransmission applies.
    Parity,
    /// Any other failure; the link is unusable.
    Transport(E),
}

/// Outcome of a failed [`Session::transceive`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransceiveError<E> {
    /// Protocol failure: dead link, RESYNC limit, or a card abort.
    Protocol(Error),
    /// Transport failure under the state machine.
    Transport(E),
}

impl<E> From<Error> for TransceiveError<E> {
    fn from(error: Error) -> Self {
        Self::Protocol(error)
    }
}

pub struct Session {
    edc: Edc,
    ifsc: usize,
    nad: u8,
    send_sequence: u8,
    receive_sequence: u8,
    wtx: u8,
    more: bool,
    retries: u8,
    resyncs: u8,
    state: State,
    last_sent: Vec<u8>,
}

impl Session {
    pub fn new(edc: Edc, ifsc: u8) -> Result<Self> {
        if ifsc == 0 || ifsc == u8::MAX {
            return Err(Error::Invalid("T=1 IFSC"));
        }
        Ok(Self {
            edc,
            ifsc: usize::from(ifsc),
            nad: 0,
            send_sequence: 0,
            receive_sequence: 0,
            wtx: 0,
            more: false,
            retries: RETRIES,
            resyncs: MAX_RESYNCS,
            state: State::Sending,
            last_sent: Vec::new(),
        })
    }

    #[must_use]
    pub const fn edc(&self) -> Edc {
        self.edc
    }

    #[must_use]
    pub const fn state(&self) -> State {
        self.state
    }

    /// Re-arm the link after a failed exchange. The card itself is not reset.
    pub fn reset(&mut self) {
        self.send_sequence = 0;
        self.receive_sequence = 0;
        self.wtx = 0;
        self.more = false;
        self.retries = RETRIES;
        self.resyncs = MAX_RESYNCS;
        self.state = State::Sending;
        self.last_sent.clear();
    }

    /// Run one complete T=1 exchange, following libccid's `t1_transceive`.
    ///
    /// `exchange` transmits one encoded block (with the current WTX as the
    /// block waiting time extension) and returns the card's raw frame.
    pub fn transceive<E, F>(
        &mut self,
        request: &[u8],
        mut exchange: F,
    ) -> core::result::Result<Vec<u8>, TransceiveError<E>>
    where
        F: FnMut(&[u8], u8) -> core::result::Result<Vec<u8>, ExchangeError<E>>,
    {
        if request.is_empty() {
            return Err(TransceiveError::Protocol(Error::Invalid(
                "T=1 request is empty",
            )));
        }
        if self.state == State::Dead {
            return Err(TransceiveError::Protocol(Error::Invalid(
                "T=1 link is dead",
            )));
        }

        self.state = State::Sending;
        self.retries = RETRIES;
        self.resyncs = MAX_RESYNCS;
        self.last_sent.clear();

        let mut sent = 0_usize;
        let mut frames = MAX_EXCHANGE_FRAMES;
        let mut received = Vec::new();
        let (mut block, mut chunk) = self.information_block(request, sent)?;
        self.last_sent.clone_from(&block);

        'exchange: loop {
            if frames == 0 {
                self.state = State::Dead;
                return Err(TransceiveError::Protocol(Error::Invalid(
                    "T=1 exchange exceeded its frame budget",
                )));
            }
            frames -= 1;
            self.retries = self.retries.saturating_sub(1);
            let frame = match exchange(&block, self.wtx) {
                Ok(frame) => frame,
                Err(ExchangeError::Parity) => match self.recover(ReceiveError::Edc) {
                    Ok(next) => {
                        block = next;
                        continue 'exchange;
                    }
                    Err(error) => return Err(TransceiveError::Protocol(error)),
                },
                Err(ExchangeError::Transport(error)) => {
                    self.state = State::Dead;
                    return Err(TransceiveError::Transport(error));
                }
            };

            let decoded = Block::try_decode(&frame, self.edc);
            let incoming = match decoded {
                Ok(incoming) if incoming.nad == self.nad => incoming,
                other => {
                    let error = match other {
                        Err(DecodeError::Edc) => ReceiveError::Edc,
                        _ => ReceiveError::Other,
                    };
                    match self.recover(error) {
                        Ok(next) => {
                            block = next;
                            continue 'exchange;
                        }
                        Err(error) => return Err(TransceiveError::Protocol(error)),
                    }
                }
            };

            match incoming.kind {
                BlockKind::ReceiveReady { sequence, .. } => {
                    // An R-block is NAD, PCB, LEN=0 and the EDC; PCB bit 5 is
                    // reserved.
                    if !incoming.information.is_empty() || frame[1] & 0x20 != 0 {
                        match self.recover(ReceiveError::Other) {
                            Ok(next) => {
                                block = next;
                                continue 'exchange;
                            }
                            Err(error) => return Err(TransceiveError::Protocol(error)),
                        }
                    }
                    if sequence != self.send_sequence && !self.more {
                        match self.recover(ReceiveError::Other) {
                            Ok(next) => {
                                block = next;
                                continue 'exchange;
                            }
                            Err(error) => return Err(TransceiveError::Protocol(error)),
                        }
                    }
                    if self.state == State::Receiving {
                        // We were receiving; answer with a plain R-block.
                        block = Block::receive_ready(self.nad, self.receive_sequence, None)
                            .encode(self.edc)?;
                        self.last_sent.clone_from(&block);
                        self.retries = RETRIES;
                        continue 'exchange;
                    }
                    // In SENDING state the R-block acknowledges our block.
                    if sequence != self.send_sequence {
                        sent += chunk;
                        self.send_sequence ^= 1;
                    }
                    if sent >= request.len() {
                        // Nothing left to send and the card answered with an
                        // R-block: there is no response data to collect.
                        self.state = State::Dead;
                        return Err(TransceiveError::Protocol(Error::Invalid(
                            "T=1 card acknowledged without a response",
                        )));
                    }
                    let (next, consumed) = self.information_block(request, sent)?;
                    chunk = consumed;
                    self.last_sent.clone_from(&next);
                    block = next;
                    self.retries = RETRIES;
                    continue 'exchange;
                }
                BlockKind::Information { sequence, more } => {
                    if self.state == State::Sending {
                        sent += chunk;
                        self.send_sequence ^= 1;
                    }
                    self.state = State::Receiving;
                    if sequence != self.receive_sequence {
                        match self.recover(ReceiveError::Other) {
                            Ok(next) => {
                                block = next;
                                continue 'exchange;
                            }
                            Err(error) => return Err(TransceiveError::Protocol(error)),
                        }
                    }
                    self.receive_sequence ^= 1;
                    received.extend_from_slice(&incoming.information);
                    if received.len() > MAX_EXCHANGE_BYTES {
                        self.state = State::Dead;
                        return Err(TransceiveError::Protocol(Error::Invalid(
                            "T=1 response exceeded its byte budget",
                        )));
                    }
                    if !more {
                        return Ok(received);
                    }
                    block = Block::receive_ready(self.nad, self.receive_sequence, None)
                        .encode(self.edc)?;
                    self.last_sent.clone_from(&block);
                    self.retries = RETRIES;
                    continue 'exchange;
                }
                BlockKind::Supervisory { response, kind } => {
                    if response {
                        if self.state != State::Resync {
                            match self.recover(ReceiveError::Other) {
                                Ok(next) => {
                                    block = next;
                                    continue 'exchange;
                                }
                                Err(error) => return Err(TransceiveError::Protocol(error)),
                            }
                        }
                        // RESYNC accepted (rule 6.2): restart the exchange.
                        self.state = State::Sending;
                        self.resyncs = MAX_RESYNCS;
                        self.retries = RETRIES;
                        received.clear();
                        sent = 0;
                        let (next, consumed) = self.information_block(request, 0)?;
                        chunk = consumed;
                        self.last_sent.clone_from(&next);
                        block = next;
                        continue 'exchange;
                    }
                    match kind {
                        SupervisoryKind::Resync => {
                            if !incoming.information.is_empty() {
                                match self.recover(ReceiveError::Other) {
                                    Ok(next) => {
                                        block = next;
                                        continue 'exchange;
                                    }
                                    Err(error) => return Err(TransceiveError::Protocol(error)),
                                }
                            }
                            match self.resync() {
                                Ok(next) => {
                                    block = next;
                                    continue 'exchange;
                                }
                                Err(error) => return Err(TransceiveError::Protocol(error)),
                            }
                        }
                        SupervisoryKind::Abort => {
                            if !incoming.information.is_empty() {
                                match self.recover(ReceiveError::Other) {
                                    Ok(next) => {
                                        block = next;
                                        continue 'exchange;
                                    }
                                    Err(error) => return Err(TransceiveError::Protocol(error)),
                                }
                            }
                            self.state = State::Dead;
                            return Err(TransceiveError::Protocol(Error::Invalid(
                                "T=1 card requested abort",
                            )));
                        }
                        SupervisoryKind::Ifs => {
                            if incoming.information.len() != 1 {
                                match self.recover(ReceiveError::Other) {
                                    Ok(next) => {
                                        block = next;
                                        continue 'exchange;
                                    }
                                    Err(error) => return Err(TransceiveError::Protocol(error)),
                                }
                            }
                            let value = incoming.information[0];
                            if value == 0 || value == u8::MAX {
                                match self.resync() {
                                    Ok(next) => {
                                        block = next;
                                        continue 'exchange;
                                    }
                                    Err(error) => return Err(TransceiveError::Protocol(error)),
                                }
                            }
                            self.ifsc = usize::from(value);
                            block =
                                Block::supervisory(self.nad, true, SupervisoryKind::Ifs, &[value])
                                    .encode(self.edc)?;
                            self.last_sent.clone_from(&block);
                            self.retries = RETRIES;
                            continue 'exchange;
                        }
                        SupervisoryKind::WaitingTimeExtension => {
                            if incoming.information.len() != 1 {
                                match self.recover(ReceiveError::Other) {
                                    Ok(next) => {
                                        block = next;
                                        continue 'exchange;
                                    }
                                    Err(error) => return Err(TransceiveError::Protocol(error)),
                                }
                            }
                            let value = incoming.information[0];
                            self.wtx = value;
                            block = Block::supervisory(
                                self.nad,
                                true,
                                SupervisoryKind::WaitingTimeExtension,
                                &[value],
                            )
                            .encode(self.edc)?;
                            self.last_sent.clone_from(&block);
                            self.retries = RETRIES;
                            continue 'exchange;
                        }
                    }
                }
            }
        }
    }

    /// Next block after a rejected or unusable block: retransmit while
    /// retries remain, otherwise run the RESYNC procedure (rule 6.4).
    fn recover(&mut self, error: ReceiveError) -> Result<Vec<u8>> {
        if self.retries > 0 {
            return self.retransmission(error);
        }
        self.resync()
    }

    fn retransmission(&self, error: ReceiveError) -> Result<Vec<u8>> {
        if self.last_sent_is_receive_ready() {
            return Ok(self.last_sent.clone());
        }
        Block::receive_ready(self.nad, self.receive_sequence, Some(error)).encode(self.edc)
    }

    fn resync(&mut self) -> Result<Vec<u8>> {
        if self.resyncs == 0 {
            self.state = State::Dead;
            return Err(Error::Invalid("T=1 RESYNC limit exceeded"));
        }
        self.resyncs -= 1;
        self.send_sequence = 0;
        self.receive_sequence = 0;
        self.more = false;
        self.state = State::Resync;
        Block::supervisory(self.nad, false, SupervisoryKind::Resync, &[]).encode(self.edc)
    }

    fn last_sent_is_receive_ready(&self) -> bool {
        self.last_sent.get(1).is_some_and(|pcb| pcb & 0xc0 == 0x80)
    }

    fn information_block(&mut self, request: &[u8], offset: usize) -> Result<(Vec<u8>, usize)> {
        let remaining = request
            .get(offset..)
            .ok_or(Error::Invalid("T=1 send offset"))?;
        if remaining.is_empty() {
            return Err(Error::Invalid("T=1 command already sent"));
        }
        let length = remaining.len().min(self.ifsc);
        let more = length < remaining.len();
        self.more = more;
        let block = Block::information(self.nad, self.send_sequence, more, &remaining[..length])
            .encode(self.edc)?;
        Ok((block, length))
    }
}
