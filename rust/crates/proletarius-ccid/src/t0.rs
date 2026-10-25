//! ISO/IEC 7816-3 T=0 character-level procedure-byte state machine.

use alloc::vec::Vec;

use crate::{Error, Result};

// Some USB CCID readers emit a long run of NULL procedure bytes while an
// eUICC prepares EF.DIR/profile data.  1024 steps was below what those cards
// need and surfaced as the misleading "length exceeds" error.  The USB
// exchange deadline remains the hard time bound; this is only a frame-count
// guard against a stuck reader.
const MAX_PROCEDURE_STEPS: usize = 65_535;
// T=0 short APDUs can return a full 256-byte data block plus SW1/SW2.  Keep
// the guard aligned with the ISO short-response maximum rather than the old
// 258-byte incidental buffer limit; larger eUICC responses are retrieved by
// the caller with GET RESPONSE rounds.
const MAX_RESPONSE: usize = 256 + 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Exchange {
    pub data: Vec<u8>,
    pub expected: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Progress {
    Exchange(Exchange),
    Complete(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Idle,
    Procedure,
    ReceiveData(usize),
    Status2(u8),
}

pub struct Session {
    state: State,
    ins: u8,
    outgoing: Vec<u8>,
    sent: usize,
    expected_data: usize,
    response: Vec<u8>,
    steps: usize,
}

impl Session {
    pub const fn new() -> Self {
        Self {
            state: State::Idle,
            ins: 0,
            outgoing: Vec::new(),
            sent: 0,
            expected_data: 0,
            response: Vec::new(),
            steps: 0,
        }
    }

    pub fn start(&mut self, command: &[u8]) -> Result<Exchange> {
        if self.state != State::Idle || !(4..=261).contains(&command.len()) {
            return Err(Error::Invalid("T=0 APDU state or length"));
        }
        let mut header = [0_u8; 5];
        header[..4].copy_from_slice(&command[..4]);
        self.ins = command[1];
        if self.ins & 0xf0 == 0x60 || self.ins & 0xf0 == 0x90 {
            return Err(Error::Invalid("T=0 INS conflicts with status byte"));
        }
        match command.len() {
            4 => {}
            5 => {
                header[4] = command[4];
                self.expected_data = decode_short_length(command[4]);
            }
            _ => {
                let lc = usize::from(command[4]);
                if lc == 0
                    || !matches!(command.len(), length if length == 5 + lc || length == 6 + lc)
                {
                    return Err(Error::Unsupported("T=0 extended or malformed APDU"));
                }
                header[4] = command[4];
                self.outgoing.extend_from_slice(&command[5..5 + lc]);
                if command.len() == 6 + lc {
                    self.expected_data = decode_short_length(command[5 + lc]);
                }
            }
        }
        self.state = State::Procedure;
        Ok(Exchange {
            data: header.to_vec(),
            expected: 1,
        })
    }

    pub fn handle(&mut self, input: &[u8]) -> Result<Progress> {
        self.steps = self.steps.checked_add(1).ok_or(Error::LengthOverflow)?;
        if self.steps > MAX_PROCEDURE_STEPS {
            return self.fail(Error::Invalid("T=0 procedure step limit exceeded"));
        }
        let result = match self.state {
            State::Idle => Err(Error::Invalid("T=0 session is idle")),
            State::Procedure => self.handle_procedure(input),
            State::ReceiveData(expected) => self.handle_data(input, expected),
            State::Status2(sw1) => self.handle_status2(input, sw1),
        };
        if result.is_err() {
            self.reset();
        }
        result
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.ins = 0;
        self.outgoing.clear();
        self.sent = 0;
        self.expected_data = 0;
        self.response.clear();
        self.steps = 0;
    }

    fn handle_procedure(&mut self, input: &[u8]) -> Result<Progress> {
        let [procedure] = input else {
            return Err(Error::Invalid("T=0 procedure-byte response length"));
        };
        if *procedure == 0x60 {
            return Ok(Progress::Exchange(receive(1)));
        }
        if procedure & 0xf0 == 0x60 || procedure & 0xf0 == 0x90 {
            self.state = State::Status2(*procedure);
            return Ok(Progress::Exchange(receive(1)));
        }
        if *procedure == self.ins || *procedure == self.ins ^ 0x01 {
            if self.sent < self.outgoing.len() {
                let data = self.outgoing[self.sent..].to_vec();
                self.sent = self.outgoing.len();
                return Ok(Progress::Exchange(Exchange { data, expected: 1 }));
            }
            if self.response.len() < self.expected_data {
                let remaining = self.expected_data - self.response.len();
                self.state = State::ReceiveData(remaining);
                return Ok(Progress::Exchange(receive(remaining)));
            }
            return Err(Error::Invalid("T=0 ACK has no pending data"));
        }
        if *procedure == self.ins ^ 0xff || *procedure == self.ins ^ 0xfe {
            if self.sent < self.outgoing.len() {
                let data = alloc::vec![self.outgoing[self.sent]];
                self.sent += 1;
                return Ok(Progress::Exchange(Exchange { data, expected: 1 }));
            }
            if self.response.len() < self.expected_data {
                self.state = State::ReceiveData(1);
                return Ok(Progress::Exchange(receive(1)));
            }
            return Err(Error::Invalid("T=0 single-byte ACK has no pending data"));
        }
        Err(Error::Invalid("unrecognized T=0 procedure byte"))
    }

    fn handle_data(&mut self, input: &[u8], expected: usize) -> Result<Progress> {
        if input.len() != expected || self.response.len() + input.len() > MAX_RESPONSE - 2 {
            return Err(Error::Invalid("T=0 response data length"));
        }
        self.response.extend_from_slice(input);
        self.state = State::Procedure;
        Ok(Progress::Exchange(receive(1)))
    }

    fn handle_status2(&mut self, input: &[u8], sw1: u8) -> Result<Progress> {
        let [sw2] = input else {
            return Err(Error::Invalid("T=0 SW2 response length"));
        };
        self.response.extend_from_slice(&[sw1, *sw2]);
        let response = core::mem::take(&mut self.response);
        self.reset();
        Ok(Progress::Complete(response))
    }

    fn fail<T>(&mut self, error: Error) -> Result<T> {
        self.reset();
        Err(error)
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

fn decode_short_length(value: u8) -> usize {
    if value == 0 { 256 } else { usize::from(value) }
}

fn receive(length: usize) -> Exchange {
    Exchange {
        data: Vec::new(),
        expected: u16::try_from(length).unwrap_or(u16::MAX),
    }
}
