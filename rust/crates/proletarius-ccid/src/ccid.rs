// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! CCID bulk-transfer framing, independent of the USBFS transport.

use alloc::vec::Vec;

use crate::{Error, Result, checked_extend};

pub const CCID_HEADER_LEN: usize = 10;
pub const PC_TO_RDR_SET_PARAMETERS: u8 = 0x61;
pub const PC_TO_RDR_ICC_POWER_ON: u8 = 0x62;
pub const PC_TO_RDR_ICC_POWER_OFF: u8 = 0x63;
pub const PC_TO_RDR_GET_SLOT_STATUS: u8 = 0x65;
pub const PC_TO_RDR_XFR_BLOCK: u8 = 0x6f;
pub const PC_TO_RDR_ABORT: u8 = 0x72;
pub const PC_TO_RDR_SET_DATA_RATE_CLOCK: u8 = 0x73;
pub const RDR_TO_PC_DATA_BLOCK: u8 = 0x80;
pub const RDR_TO_PC_SLOT_STATUS: u8 = 0x81;
pub const RDR_TO_PC_PARAMETERS: u8 = 0x82;
pub const RDR_TO_PC_DATA_RATE_CLOCK: u8 = 0x84;
pub const COMMAND_STATUS_MASK: u8 = 0xc0;
pub const COMMAND_FAILED: u8 = 0x40;
pub const COMMAND_TIME_EXTENSION: u8 = 0x80;
pub const ICC_STATUS_MASK: u8 = 0x03;
pub const ICC_ACTIVE: u8 = 0x00;
pub const ICC_INACTIVE: u8 = 0x01;
pub const ICC_ABSENT: u8 = 0x02;
pub const MAX_CCID_PAYLOAD: usize = 65_535;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header {
    pub message_type: u8,
    pub length: u32,
    pub slot: u8,
    pub sequence: u8,
    pub parameter0: u8,
    pub parameter1: u8,
    pub parameter2: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataBlock<'a> {
    pub header: Header,
    pub status: u8,
    pub error: u8,
    pub chain_parameter: u8,
    pub data: &'a [u8],
}

pub fn encode_command(
    message_type: u8,
    slot: u8,
    sequence: u8,
    parameters: [u8; 3],
    data: &[u8],
    output: &mut Vec<u8>,
) -> Result<()> {
    if data.len() > MAX_CCID_PAYLOAD {
        return Err(Error::LengthOverflow);
    }
    let length = u32::try_from(data.len()).map_err(|_| Error::LengthOverflow)?;
    output.extend_from_slice(&[
        message_type,
        length as u8,
        (length >> 8) as u8,
        (length >> 16) as u8,
        (length >> 24) as u8,
        slot,
        sequence,
        parameters[0],
        parameters[1],
        parameters[2],
    ]);
    checked_extend(output, data)
}

pub fn encode_xfr_block(
    slot: u8,
    sequence: u8,
    level_parameter: u8,
    apdu: &[u8],
    output: &mut Vec<u8>,
) -> Result<()> {
    encode_command(
        PC_TO_RDR_XFR_BLOCK,
        slot,
        sequence,
        [level_parameter, 0, 0],
        apdu,
        output,
    )
}

pub fn decode_response(input: &[u8]) -> Result<DataBlock<'_>> {
    if input.len() < CCID_HEADER_LEN {
        return Err(Error::Truncated);
    }
    let length = u32::from_le_bytes([input[1], input[2], input[3], input[4]]) as usize;
    let end = CCID_HEADER_LEN
        .checked_add(length)
        .ok_or(Error::LengthOverflow)?;
    if end > input.len() {
        return Err(Error::Truncated);
    }
    let header = Header {
        message_type: input[0],
        length: length as u32,
        slot: input[5],
        sequence: input[6],
        parameter0: input[7],
        parameter1: input[8],
        parameter2: input[9],
    };
    if !matches!(
        header.message_type,
        RDR_TO_PC_DATA_BLOCK
            | RDR_TO_PC_SLOT_STATUS
            | RDR_TO_PC_PARAMETERS
            | RDR_TO_PC_DATA_RATE_CLOCK
    ) {
        return Err(Error::Invalid("unexpected CCID response type"));
    }
    if header.message_type == RDR_TO_PC_SLOT_STATUS {
        if length != 0 {
            return Err(Error::Invalid("slot-status CCID response has data"));
        }
        return Ok(DataBlock {
            header,
            status: input[7],
            error: input[8],
            chain_parameter: input[9],
            data: &[],
        });
    }
    Ok(DataBlock {
        header,
        status: input[7],
        error: input[8],
        chain_parameter: input[9],
        data: &input[CCID_HEADER_LEN..end],
    })
}

pub fn decode_data_block(input: &[u8]) -> Result<DataBlock<'_>> {
    let response = decode_response(input)?;
    if !matches!(
        response.header.message_type,
        RDR_TO_PC_DATA_BLOCK | RDR_TO_PC_SLOT_STATUS
    ) {
        return Err(Error::Invalid("unexpected CCID data response type"));
    }
    Ok(response)
}
