// Copyright (c) 2026 iniwex5
// Modifications copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

#![no_std]

use core::{error::Error, fmt, iter::FusedIterator};

pub const QMUX_HEADER_LEN: usize = 6;
pub const QRTR_VIRTUAL_HEADER_LEN: usize = 7;
pub const CONTROL_HEADER_LEN: usize = 6;
pub const SERVICE_HEADER_LEN: usize = 7;
pub const TLV_HEADER_LEN: usize = 3;
pub const CONTROL_SERVICE: u16 = 0;
pub const RESULT_TLV_KIND: u8 = 0x02;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameKind {
    Qmux,
    QrtrVirtual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketMeta {
    pub service: u16,
    pub client_id: u8,
    pub transaction_id: u16,
    pub message_id: u16,
    pub frame_control_flags: u8,
    pub message_control_flags: u8,
}

impl PacketMeta {
    #[must_use]
    pub const fn request(
        service: u16,
        client_id: u8,
        transaction_id: u16,
        message_id: u16,
    ) -> Self {
        Self {
            service,
            client_id,
            transaction_id,
            message_id,
            frame_control_flags: 0,
            message_control_flags: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tlv<'a> {
    pub kind: u8,
    pub value: &'a [u8],
}

impl<'a> Tlv<'a> {
    #[must_use]
    pub const fn new(kind: u8, value: &'a [u8]) -> Self {
        Self { kind, value }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultCode {
    pub result: u16,
    pub error: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedPacket<'a> {
    pub frame_kind: FrameKind,
    pub meta: PacketMeta,
    tlv_bytes: &'a [u8],
}

impl<'a> DecodedPacket<'a> {
    #[must_use]
    pub const fn tlv_bytes(&self) -> &'a [u8] {
        self.tlv_bytes
    }

    #[must_use]
    pub const fn tlvs(&self) -> TlvIter<'a> {
        TlvIter {
            remaining: self.tlv_bytes,
        }
    }

    #[must_use]
    pub fn find_tlv(&self, kind: u8) -> Option<Tlv<'a>> {
        self.tlvs().find(|tlv| tlv.kind == kind)
    }

    #[must_use]
    pub const fn is_indication(&self) -> bool {
        if self.meta.service == CONTROL_SERVICE {
            self.meta.message_control_flags & 0x02 != 0
        } else {
            self.meta.message_control_flags & 0x04 != 0
        }
    }

    #[must_use]
    pub fn result_code(&self) -> Option<ResultCode> {
        let value = self.find_tlv(RESULT_TLV_KIND)?.value;
        if value.len() < 4 {
            return None;
        }
        Some(ResultCode {
            result: read_u16(value, 0),
            error: read_u16(value, 2),
        })
    }

    #[must_use]
    pub fn is_success(&self) -> bool {
        self.result_code().is_some_and(|result| result.result == 0)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TlvIter<'a> {
    remaining: &'a [u8],
}

impl<'a> TlvIter<'a> {
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { remaining: data }
    }
}

impl<'a> Iterator for TlvIter<'a> {
    type Item = Tlv<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.len() < TLV_HEADER_LEN {
            self.remaining = &[];
            return None;
        }

        let value_len = usize::from(read_u16(self.remaining, 1));
        let consumed = TLV_HEADER_LEN + value_len;
        if consumed > self.remaining.len() {
            self.remaining = &[];
            return None;
        }
        let tlv = Tlv {
            kind: self.remaining[0],
            value: &self.remaining[TLV_HEADER_LEN..consumed],
        };
        self.remaining = &self.remaining[consumed..];
        Some(tlv)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.remaining.len() / TLV_HEADER_LEN))
    }
}

impl FusedIterator for TlvIter<'_> {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    EmptyFrame,
    InvalidFrameMarker(u8),
    TruncatedFrameHeader {
        needed: usize,
        available: usize,
    },
    InvalidFrameLength {
        declared: u16,
        minimum: usize,
    },
    TruncatedFrame {
        needed: usize,
        available: usize,
    },
    TruncatedMessageHeader {
        needed: usize,
        available: usize,
    },
    TruncatedMessage {
        needed: usize,
        available: usize,
    },
    TruncatedTlvHeader {
        available: usize,
    },
    TruncatedTlvValue {
        kind: u8,
        needed: usize,
        available: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFrame => f.write_str("empty QMI frame"),
            Self::InvalidFrameMarker(marker) => {
                write!(f, "invalid QMI frame marker 0x{marker:02x}")
            }
            Self::TruncatedFrameHeader { needed, available } => write!(
                f,
                "truncated QMI frame header: need {needed} bytes, have {available}"
            ),
            Self::InvalidFrameLength { declared, minimum } => write!(
                f,
                "invalid QMI frame length {declared}: minimum is {minimum}"
            ),
            Self::TruncatedFrame { needed, available } => write!(
                f,
                "truncated QMI frame: need {needed} bytes, have {available}"
            ),
            Self::TruncatedMessageHeader { needed, available } => write!(
                f,
                "truncated QMI message header: need {needed} bytes, have {available}"
            ),
            Self::TruncatedMessage { needed, available } => write!(
                f,
                "truncated QMI message: need {needed} TLV bytes, have {available}"
            ),
            Self::TruncatedTlvHeader { available } => {
                write!(f, "truncated QMI TLV header: have {available} bytes")
            }
            Self::TruncatedTlvValue {
                kind,
                needed,
                available,
            } => write!(
                f,
                "truncated QMI TLV 0x{kind:02x}: need {needed} bytes, have {available}"
            ),
        }
    }
}

impl Error for DecodeError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    TlvValueTooLong { kind: u8, length: usize },
    TlvSectionTooLong(usize),
    FrameTooLong(usize),
    OutputTooSmall { needed: usize, available: usize },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TlvValueTooLong { kind, length } => {
                write!(f, "QMI TLV 0x{kind:02x} value is too long: {length} bytes")
            }
            Self::TlvSectionTooLong(length) => {
                write!(f, "QMI TLV section is too long: {length} bytes")
            }
            Self::FrameTooLong(length) => write!(f, "QMI frame is too long: {length} bytes"),
            Self::OutputTooSmall { needed, available } => write!(
                f,
                "QMI output buffer is too small: need {needed} bytes, have {available}"
            ),
        }
    }
}

impl Error for EncodeError {}

pub fn decode_packet(data: &[u8]) -> Result<DecodedPacket<'_>, DecodeError> {
    let marker = *data.first().ok_or(DecodeError::EmptyFrame)?;
    let (frame_kind, frame_header_len) = match marker {
        0x01 => (FrameKind::Qmux, QMUX_HEADER_LEN),
        0x02 => (FrameKind::QrtrVirtual, QRTR_VIRTUAL_HEADER_LEN),
        _ => return Err(DecodeError::InvalidFrameMarker(marker)),
    };
    if data.len() < frame_header_len {
        return Err(DecodeError::TruncatedFrameHeader {
            needed: frame_header_len,
            available: data.len(),
        });
    }

    let declared_frame_len = read_u16(data, 1);
    let frame_len = usize::from(declared_frame_len) + 1;
    if frame_len < frame_header_len {
        return Err(DecodeError::InvalidFrameLength {
            declared: declared_frame_len,
            minimum: frame_header_len - 1,
        });
    }
    if data.len() < frame_len {
        return Err(DecodeError::TruncatedFrame {
            needed: frame_len,
            available: data.len(),
        });
    }

    let frame = &data[..frame_len];
    let service = if frame_kind == FrameKind::Qmux {
        u16::from(frame[4])
    } else {
        read_u16(frame, 4)
    };
    let client_id = if frame_kind == FrameKind::Qmux {
        frame[5]
    } else {
        frame[6]
    };
    let body = &frame[frame_header_len..];
    let message_header_len = if service == CONTROL_SERVICE {
        CONTROL_HEADER_LEN
    } else {
        SERVICE_HEADER_LEN
    };
    if body.len() < message_header_len {
        return Err(DecodeError::TruncatedMessageHeader {
            needed: message_header_len,
            available: body.len(),
        });
    }

    let (message_control_flags, transaction_id, message_id, tlv_len) = if service == CONTROL_SERVICE
    {
        (
            body[0],
            u16::from(body[1]),
            read_u16(body, 2),
            usize::from(read_u16(body, 4)),
        )
    } else {
        (
            body[0],
            read_u16(body, 1),
            read_u16(body, 3),
            usize::from(read_u16(body, 5)),
        )
    };
    let available_tlv_bytes = body.len() - message_header_len;
    if available_tlv_bytes < tlv_len {
        return Err(DecodeError::TruncatedMessage {
            needed: tlv_len,
            available: available_tlv_bytes,
        });
    }
    let tlv_bytes = &body[message_header_len..message_header_len + tlv_len];
    validate_tlvs(tlv_bytes)?;

    Ok(DecodedPacket {
        frame_kind,
        meta: PacketMeta {
            service,
            client_id,
            transaction_id,
            message_id,
            frame_control_flags: frame[3],
            message_control_flags,
        },
        tlv_bytes,
    })
}

pub fn encoded_len(meta: PacketMeta, tlvs: &[Tlv<'_>]) -> Result<usize, EncodeError> {
    let tlv_len = tlv_bytes_len(tlvs)?;
    let frame_header_len = if meta.service <= u16::from(u8::MAX) {
        QMUX_HEADER_LEN
    } else {
        QRTR_VIRTUAL_HEADER_LEN
    };
    let message_header_len = if meta.service == CONTROL_SERVICE {
        CONTROL_HEADER_LEN
    } else {
        SERVICE_HEADER_LEN
    };
    let total = frame_header_len + message_header_len + tlv_len;
    if total - 1 > usize::from(u16::MAX) {
        return Err(EncodeError::FrameTooLong(total));
    }
    Ok(total)
}

pub fn encode_packet(
    meta: PacketMeta,
    tlvs: &[Tlv<'_>],
    output: &mut [u8],
) -> Result<usize, EncodeError> {
    let total = encoded_len(meta, tlvs)?;
    if output.len() < total {
        return Err(EncodeError::OutputTooSmall {
            needed: total,
            available: output.len(),
        });
    }

    let tlv_len = tlv_bytes_len(tlvs)?;
    let frame_header_len = if meta.service <= u16::from(u8::MAX) {
        output[0] = 0x01;
        output[4] = meta.service as u8;
        output[5] = meta.client_id;
        QMUX_HEADER_LEN
    } else {
        output[0] = 0x02;
        write_u16(output, 4, meta.service);
        output[6] = meta.client_id;
        QRTR_VIRTUAL_HEADER_LEN
    };
    write_u16(output, 1, (total - 1) as u16);
    output[3] = meta.frame_control_flags;

    let body = &mut output[frame_header_len..total];
    let message_header_len = if meta.service == CONTROL_SERVICE {
        body[0] = meta.message_control_flags;
        body[1] = meta.transaction_id as u8;
        write_u16(body, 2, meta.message_id);
        write_u16(body, 4, tlv_len as u16);
        CONTROL_HEADER_LEN
    } else {
        body[0] = meta.message_control_flags;
        write_u16(body, 1, meta.transaction_id);
        write_u16(body, 3, meta.message_id);
        write_u16(body, 5, tlv_len as u16);
        SERVICE_HEADER_LEN
    };

    let mut offset = message_header_len;
    for tlv in tlvs {
        body[offset] = tlv.kind;
        write_u16(body, offset + 1, tlv.value.len() as u16);
        offset += TLV_HEADER_LEN;
        body[offset..offset + tlv.value.len()].copy_from_slice(tlv.value);
        offset += tlv.value.len();
    }
    Ok(total)
}

fn validate_tlvs(mut data: &[u8]) -> Result<(), DecodeError> {
    while !data.is_empty() {
        if data.len() < TLV_HEADER_LEN {
            if data.iter().all(|byte| *byte == 0) {
                return Ok(());
            }
            return Err(DecodeError::TruncatedTlvHeader {
                available: data.len(),
            });
        }

        let value_len = usize::from(read_u16(data, 1));
        let available = data.len() - TLV_HEADER_LEN;
        if available < value_len {
            return Err(DecodeError::TruncatedTlvValue {
                kind: data[0],
                needed: value_len,
                available,
            });
        }
        data = &data[TLV_HEADER_LEN + value_len..];
    }
    Ok(())
}

fn tlv_bytes_len(tlvs: &[Tlv<'_>]) -> Result<usize, EncodeError> {
    let mut total = 0usize;
    for tlv in tlvs {
        if tlv.value.len() > usize::from(u16::MAX) {
            return Err(EncodeError::TlvValueTooLong {
                kind: tlv.kind,
                length: tlv.value.len(),
            });
        }
        total = total
            .checked_add(TLV_HEADER_LEN + tlv.value.len())
            .ok_or(EncodeError::TlvSectionTooLong(usize::MAX))?;
        if total > usize::from(u16::MAX) {
            return Err(EncodeError::TlvSectionTooLong(total));
        }
    }
    Ok(total)
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn write_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
