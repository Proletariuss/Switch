// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! QMAP framing: how a module multiplexes its data sessions over one pipe.
//!
//! When a WDS call is bound to a mux, the module prefixes every packet with
//! the map header below. Linux demultiplexes it in the kernel (`rmnet`), which
//! is why this codec has no counterpart there; on Windows the host reads the
//! module's bulk pipe itself, so the framing has to be understood in user
//! space.
//!
//! ```text
//! struct rmnet_map_header {   /* include/linux/if_rmnet.h */
//!     u8 flags;       /* bit 7 CMD, bit 6 NEXT_HEADER, bits 5..0 pad length */
//!     u8 mux_id;
//!     __be16 pkt_len; /* packet plus padding, big endian */
//! } __aligned(1);
//! ```

/// Bytes of the map header in front of every frame.
pub const MAP_HEADER_LEN: usize = 4;
/// Bytes of the optional version-5 checksum header a frame may carry next.
pub const MAP_V5_HEADER_LEN: usize = 4;
/// Longest padding a frame may carry: 4-byte alignment, so 0..=3.
pub const MAP_MAX_PAD: usize = 3;

const MAP_CMD_FLAG: u8 = 0x80;
const MAP_NEXT_HEADER_FLAG: u8 = 0x40;
const MAP_PAD_LEN_MASK: u8 = 0x3f;

/// What one received frame turned out to be.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapFrame<'a> {
    /// An IP packet belonging to `mux_id`.
    Data { mux_id: u8, packet: &'a [u8] },
    /// A flow-control or acknowledgement command, which the caller skips:
    /// the module sends them unasked and they carry no payload for the host.
    Command { mux_id: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapError {
    /// The frame is shorter than its own header claims.
    Truncated { needed: usize, available: usize },
    /// The header cannot describe a packet.
    Invalid,
}

impl std::fmt::Display for MapError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { needed, available } => write!(
                formatter,
                "QMAP frame is truncated: {needed} bytes needed, {available} available"
            ),
            Self::Invalid => formatter.write_str("QMAP header is invalid"),
        }
    }
}

impl std::error::Error for MapError {}

/// Length of the frame `packet` becomes, header and padding included.
#[must_use]
pub const fn encoded_len(packet_len: usize) -> usize {
    MAP_HEADER_LEN + packet_len + padding_for(packet_len)
}

/// Padding that aligns one payload to four bytes, as the map header records.
#[must_use]
pub const fn padding_for(packet_len: usize) -> usize {
    (4 - (packet_len % 4)) % 4
}

/// Write `packet` for `mux_id` into `output`.
pub fn encode(mux_id: u8, packet: &[u8], output: &mut [u8]) -> Result<usize, MapError> {
    let total = encoded_len(packet.len());
    if output.len() < total {
        return Err(MapError::Truncated {
            needed: total,
            available: output.len(),
        });
    }
    let padding = padding_for(packet.len());
    let payload = u16::try_from(packet.len() + padding).map_err(|_| MapError::Invalid)?;
    output[0] = padding as u8;
    output[1] = mux_id;
    output[2..4].copy_from_slice(&payload.to_be_bytes());
    output[MAP_HEADER_LEN..MAP_HEADER_LEN + packet.len()].copy_from_slice(packet);
    output[MAP_HEADER_LEN + packet.len()..total].fill(0);
    Ok(total)
}

/// Read the frame in `frame`, borrowing its payload.
pub fn decode(frame: &[u8]) -> Result<MapFrame<'_>, MapError> {
    if frame.len() < MAP_HEADER_LEN {
        return Err(MapError::Truncated {
            needed: MAP_HEADER_LEN,
            available: frame.len(),
        });
    }
    let flags = frame[0];
    let mux_id = frame[1];
    let payload = usize::from(u16::from_be_bytes([frame[2], frame[3]]));
    let mut offset = MAP_HEADER_LEN;
    if flags & MAP_NEXT_HEADER_FLAG != 0 {
        // A version-5 header precedes the payload; it carries checksum state
        // the host does not need to act on.
        offset += MAP_V5_HEADER_LEN;
    }
    if flags & MAP_CMD_FLAG != 0 {
        return Ok(MapFrame::Command { mux_id });
    }
    let padding = usize::from(flags & MAP_PAD_LEN_MASK);
    // The format aligns each payload to four bytes, so a header claiming more
    // padding than that cannot describe a frame. Rejecting it here is also what
    // keeps a plain IP packet - an IPv4 header starts `0x45`, an IPv6 header
    // `0x6x`, both with low six bits above the maximum padding - from being
    // read as a map frame by a host that is looking at a firmware which does
    // not aggregate at all.
    if padding > MAP_MAX_PAD {
        return Err(MapError::Invalid);
    }
    let packet_len = payload.checked_sub(padding).ok_or(MapError::Invalid)?;
    let end = offset.checked_add(payload).ok_or(MapError::Invalid)?;
    if end > frame.len() {
        return Err(MapError::Truncated {
            needed: end,
            available: frame.len(),
        });
    }
    Ok(MapFrame::Data {
        mux_id,
        packet: &frame[offset..offset + packet_len],
    })
}
