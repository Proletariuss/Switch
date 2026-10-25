// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! Definite-length BER-TLV codec used by eUICC/LPA messages.

use alloc::vec::Vec;

use crate::{Error, Result, checked_extend};

pub const MAX_TLV_SIZE: usize = 65_535;
pub const MAX_TAG_SIZE: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tag<'a> {
    pub bytes: &'a [u8],
    pub constructed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tlv<'a> {
    pub tag: Tag<'a>,
    pub value: &'a [u8],
}

impl<'a> Tlv<'a> {
    pub fn children(self) -> Result<TlvIter<'a>> {
        if !self.tag.constructed {
            return Err(Error::Invalid("primitive TLV has no children"));
        }
        Ok(TlvIter { input: self.value })
    }

    #[must_use]
    pub fn matches(self, tag: &[u8]) -> bool {
        self.tag.bytes == tag
    }
}

pub struct TlvIter<'a> {
    input: &'a [u8],
}

impl<'a> Iterator for TlvIter<'a> {
    type Item = Result<Tlv<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.input.is_empty() {
            return None;
        }
        match parse_one(self.input) {
            Ok((tlv, consumed)) => {
                self.input = &self.input[consumed..];
                Some(Ok(tlv))
            }
            Err(error) => {
                self.input = &[];
                Some(Err(error))
            }
        }
    }
}

pub fn parse(input: &[u8]) -> Result<TlvIter<'_>> {
    // Validate lazily but reject a trailing zero-length fragment only when it
    // is actually consumed by the iterator; this keeps the iterator borrowed.
    Ok(TlvIter { input })
}

pub fn encode(tag: &[u8], value: &[u8], output: &mut Vec<u8>) -> Result<()> {
    validate_tag(tag)?;
    if value.len() > MAX_TLV_SIZE {
        return Err(Error::LengthOverflow);
    }
    checked_extend(output, tag)?;
    encode_length(value.len(), output)?;
    checked_extend(output, value)
}

pub fn encode_constructed(tag: &[u8], children: &[u8], output: &mut Vec<u8>) -> Result<()> {
    if tag.first().is_none_or(|byte| byte & 0x20 == 0) {
        return Err(Error::Invalid("constructed TLV tag is not constructed"));
    }
    encode(tag, children, output)
}

fn parse_one(input: &[u8]) -> Result<(Tlv<'_>, usize)> {
    let (tag, tag_len) = parse_tag(input)?;
    let (length, length_len) = parse_length(&input[tag_len..])?;
    let value_start = tag_len + length_len;
    let value_end = value_start
        .checked_add(length)
        .ok_or(Error::LengthOverflow)?;
    if value_end > input.len() {
        return Err(Error::Truncated);
    }
    Ok((
        Tlv {
            tag,
            value: &input[value_start..value_end],
        },
        value_end,
    ))
}

fn parse_tag(input: &[u8]) -> Result<(Tag<'_>, usize)> {
    let first = *input.first().ok_or(Error::Truncated)?;
    if first & 0x1f != 0x1f {
        return Ok((
            Tag {
                bytes: &input[..1],
                constructed: first & 0x20 != 0,
            },
            1,
        ));
    }
    let mut end = 1;
    while end < input.len() && end < MAX_TAG_SIZE {
        let byte = input[end];
        end += 1;
        if byte & 0x80 == 0 {
            return Ok((
                Tag {
                    bytes: &input[..end],
                    constructed: first & 0x20 != 0,
                },
                end,
            ));
        }
    }
    if end >= MAX_TAG_SIZE {
        Err(Error::LengthOverflow)
    } else {
        Err(Error::Truncated)
    }
}

fn parse_length(input: &[u8]) -> Result<(usize, usize)> {
    let first = *input.first().ok_or(Error::Truncated)?;
    if first & 0x80 == 0 {
        return Ok((usize::from(first), 1));
    }
    let count = usize::from(first & 0x7f);
    if count == 0 {
        return Err(Error::Unsupported("indefinite BER length"));
    }
    if count > core::mem::size_of::<usize>() || input.len() < count + 1 {
        return Err(Error::LengthOverflow);
    }
    let mut value = 0_usize;
    for byte in &input[1..=count] {
        value = value
            .checked_shl(8)
            .and_then(|value| value.checked_add(usize::from(*byte)))
            .ok_or(Error::LengthOverflow)?;
    }
    if value > MAX_TLV_SIZE {
        return Err(Error::LengthOverflow);
    }
    Ok((value, count + 1))
}

fn encode_length(length: usize, output: &mut Vec<u8>) -> Result<()> {
    if length > MAX_TLV_SIZE {
        return Err(Error::LengthOverflow);
    }
    if length < 0x80 {
        output.push(length as u8);
    } else if length <= 0xff {
        output.extend_from_slice(&[0x81, length as u8]);
    } else {
        output.extend_from_slice(&[0x82, (length >> 8) as u8, length as u8]);
    }
    Ok(())
}

fn validate_tag(tag: &[u8]) -> Result<()> {
    if tag.is_empty() || tag.len() > MAX_TAG_SIZE {
        return Err(Error::Invalid("BER tag length"));
    }
    if tag[0] & 0x1f == 0x1f && (tag.len() == 1 || tag[tag.len() - 1] & 0x80 != 0 || tag[1] == 0x80)
    {
        return Err(Error::Invalid("BER high-tag encoding"));
    }
    Ok(())
}
