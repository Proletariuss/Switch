#![deny(unsafe_op_in_unsafe_fn)]

//! Pure Bluetooth-card protocol codecs shared with the CoreBluetooth adapter.
//! GATT enumeration, Keychain storage and write pacing stay in Swift; selection,
//! credential
//! derivation, NBLE cryptography/framing and BeeSIM frame construction live here.

use des::{
    TdesEde3,
    cipher::{Block, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit},
};
use std::ffi::c_void;
use zeroize::{Zeroize, Zeroizing};

mod card_workflow;
mod deadline;
mod gatt_selection;
mod operation_ledger;
mod reconnect;
mod target_selection;

const NBLE_MAX_APDU: usize = 65_544;
const NBLE_FRAME_DATA: usize = 100;
const NBLE_MAX_RESPONSE_BODY: usize = 65_541;
const FFI_MALFORMED: isize = -1;
const FFI_CAPACITY: isize = -2;
const FFI_STALE: isize = -3;
const FFI_TIME_EXTENSION: isize = -5;

/// Forces Cargo's audio-core static archive to include this crate's Rust objects
/// so the app links one standard-library copy for all native protocol/audio code.
#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_core_link_anchor() {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum ReaderProtocol {
    Legacy = 0,
    Ccid = 1,
    BeeSim = 2,
    NineEsim = 3,
}

fn detect_reader_protocol(name: &str, has_nordic_uart: bool) -> ReaderProtocol {
    let lower = name.to_lowercase();
    if lower.contains("estkme") {
        ReaderProtocol::Ccid
    } else if lower.contains("beesim") || lower.contains("bee_sim") {
        ReaderProtocol::BeeSim
    } else if has_nordic_uart {
        ReaderProtocol::NineEsim
    } else if lower.contains("9esim") {
        // Older AE30 readers retain BeeSIM framing without Nordic UART.
        ReaderProtocol::BeeSim
    } else {
        ReaderProtocol::Legacy
    }
}

/// Name-only callers pass `has_nordic_uart = 0`. Invalid bytes and flags do
/// not select an authenticated protocol; a valid empty name may use NUS.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_detect_protocol(
    name: *const u8,
    name_length: usize,
    has_nordic_uart: u8,
) -> u8 {
    if has_nordic_uart > 1 {
        return ReaderProtocol::Legacy as u8;
    }
    let Some(name) =
        input_slice(name, name_length).and_then(|bytes| std::str::from_utf8(bytes).ok())
    else {
        return ReaderProtocol::Legacy as u8;
    };
    detect_reader_protocol(name, has_nordic_uart == 1) as u8
}

fn input_slice<'a>(pointer: *const u8, length: usize) -> Option<&'a [u8]> {
    if length == 0 {
        return Some(&[]);
    }
    if pointer.is_null() {
        return None;
    }
    // SAFETY: callers of this C ABI must provide a readable buffer of `length` bytes.
    Some(unsafe { std::slice::from_raw_parts(pointer, length) })
}

fn output_slice<'a>(pointer: *mut u8, capacity: usize) -> Option<&'a mut [u8]> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: callers of this C ABI must provide a writable buffer of `capacity` bytes.
    Some(unsafe { std::slice::from_raw_parts_mut(pointer, capacity) })
}

fn suffix(name: &[u8]) -> Option<[u8; 6]> {
    let name = std::str::from_utf8(name).ok()?.trim().as_bytes();
    let suffix = name.get(name.len().checked_sub(6)?..)?;
    if !suffix.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut result = [0; 6];
    for (target, source) in result.iter_mut().zip(suffix) {
        *target = source.to_ascii_lowercase();
    }
    Some(result)
}

fn default_password(name: &[u8]) -> Option<[u8; 6]> {
    let mut result = suffix(name)?;
    result.reverse();
    Some(result)
}

fn normalize_password(name: &[u8], password: &[u8]) -> Option<[u8; 6]> {
    let trimmed = std::str::from_utf8(password).ok()?.trim().as_bytes();
    if trimmed.is_empty() {
        return default_password(name);
    }
    if trimmed.len() == 6 && trimmed.iter().all(u8::is_ascii_hexdigit) {
        let mut normalized = [0; 6];
        for (target, source) in normalized.iter_mut().zip(trimmed) {
            *target = source.to_ascii_lowercase();
        }
        return Some(normalized);
    }
    let crc = crc24(trimmed);
    let digits = format!("{crc:06x}");
    digits.as_bytes().try_into().ok()
}

fn crc24(bytes: &[u8]) -> u32 {
    let mut crc = 0xb704ce_u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x1000000 != 0 {
                crc ^= 0x1864cfb;
            }
            crc &= 0xffffff;
        }
    }
    crc
}

fn derive_key(name: &[u8], password: &[u8]) -> Option<Zeroizing<[u8; 16]>> {
    let suffix = suffix(name)?;
    let password = normalize_password(name, password)?;
    let mut word = [0; 8];
    word[..2].copy_from_slice(&suffix[4..]);
    word[2..].copy_from_slice(&password);
    let mut key = Zeroizing::new([0; 16]);
    let mut base = Zeroizing::new([0; 4]);
    for (index, pair) in word.chunks_exact(2).enumerate() {
        base[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    key[..4].copy_from_slice(base.as_ref());
    key[4..8].copy_from_slice(base.as_ref());
    key[8..12].copy_from_slice(base.as_ref());
    Some(key)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn triple_des_block(input: &[u8], key: &[u8], decrypt: bool) -> Option<Zeroizing<[u8; 8]>> {
    if input.len() != 8 || key.len() != 16 {
        return None;
    }
    let mut expanded = Zeroizing::new([0; 24]);
    expanded[..16].copy_from_slice(key);
    expanded[16..].copy_from_slice(&key[..8]);
    let cipher = TdesEde3::new_from_slice(expanded.as_ref()).ok()?;
    let mut block = Block::<TdesEde3>::default();
    block.copy_from_slice(input);
    if decrypt {
        cipher.decrypt_block(&mut block);
    } else {
        cipher.encrypt_block(&mut block);
    }
    let mut output = Zeroizing::new([0; 8]);
    output.copy_from_slice(&block);
    block.as_mut_slice().zeroize();
    Some(output)
}

fn challenge_response(challenge: &[u8], key: &[u8]) -> Option<[u8; 17]> {
    if challenge.len() != 17 || challenge[0] != 0xe0 || key.len() != 16 {
        return None;
    }
    let mut plaintext = Zeroizing::new([0; 16]);
    let first_plaintext = triple_des_block(&challenge[1..9], key, true)?;
    let second_plaintext = triple_des_block(&challenge[9..17], key, true)?;
    plaintext[..8].copy_from_slice(first_plaintext.as_ref());
    plaintext[8..].copy_from_slice(second_plaintext.as_ref());
    let counter = u16::from_le_bytes([plaintext[7], plaintext[8]]).wrapping_add(1);
    [plaintext[7], plaintext[8]] = counter.to_le_bytes();
    let first = triple_des_block(&plaintext[..8], key, false)?;
    let second = triple_des_block(&plaintext[8..], key, false)?;
    let mut response = [0; 17];
    response[0] = 0xe0;
    response[1..9].copy_from_slice(first.as_ref());
    response[9..].copy_from_slice(second.as_ref());
    Some(response)
}

fn checksum(bytes: &[u8]) -> u16 {
    bytes
        .iter()
        .fold(0_u16, |sum, byte| sum.wrapping_add(u16::from(*byte)))
}

fn encode_nble(counter: u8, apdu: &[u8]) -> Option<Vec<Vec<u8>>> {
    if !(4..=NBLE_MAX_APDU).contains(&apdu.len()) {
        return None;
    }
    let mut body = Vec::with_capacity(apdu.len() + 3);
    body.push(counter);
    body.extend_from_slice(apdu);
    body.extend_from_slice(&checksum(&body).to_be_bytes());
    Some(
        body.chunks(NBLE_FRAME_DATA)
            .enumerate()
            .map(|(index, chunk)| {
                let final_bit = if (index + 1) * NBLE_FRAME_DATA >= body.len() {
                    0x20
                } else {
                    0
                };
                let mut frame = Vec::with_capacity(chunk.len() + 1);
                frame.push(((index as u8) & 0x1f) | final_bit);
                frame.extend_from_slice(chunk);
                frame
            })
            .collect(),
    )
}

#[derive(Default)]
struct NbleResponseDecoder {
    body: Vec<u8>,
    sequence: u8,
    complete: bool,
}

impl NbleResponseDecoder {
    fn reset(&mut self) {
        self.body.zeroize();
        self.body.clear();
        self.sequence = 0;
        self.complete = false;
    }

    fn add(&mut self, frame: &[u8], expected_counter: u8) -> Result<Option<Vec<u8>>, DecodeError> {
        if self.complete
            || frame.len() < 2
            || frame[0] & 0xc0 != 0
            || frame[0] & 0x1f != self.sequence
        {
            return Err(DecodeError::Malformed);
        }
        self.sequence = self.sequence.wrapping_add(1) & 0x1f;
        self.body.extend_from_slice(&frame[1..]);
        if self.body.len() > NBLE_MAX_RESPONSE_BODY {
            return Err(DecodeError::Malformed);
        }
        if frame[0] & 0x20 == 0 {
            return Ok(None);
        }
        self.complete = true;
        if self.body.len() < 5 {
            return Err(DecodeError::Malformed);
        }
        let checksum_offset = self.body.len() - 2;
        let expected_checksum =
            u16::from_be_bytes([self.body[checksum_offset], self.body[checksum_offset + 1]]);
        if checksum(&self.body[..checksum_offset]) != expected_checksum {
            return Err(DecodeError::Malformed);
        }
        if self.body[0] != expected_counter {
            return Err(DecodeError::Stale);
        }
        Ok(Some(self.body[1..checksum_offset].to_vec()))
    }
}

impl Drop for NbleResponseDecoder {
    fn drop(&mut self) {
        self.body.zeroize();
    }
}

#[derive(Default)]
struct BeeSimResponseDecoder {
    body: Vec<u8>,
    total: u8,
    next_index: u8,
    complete: bool,
}

impl BeeSimResponseDecoder {
    fn reset(&mut self) {
        self.body.zeroize();
        self.body.clear();
        self.total = 0;
        self.next_index = 0;
        self.complete = false;
    }

    fn add(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, ()> {
        if frame.len() < 2 || frame[0] == 0 || frame[1] == 0 || frame[1] > frame[0] {
            return Err(());
        }
        let total = frame[0];
        let index = frame[1];
        if index == 1 {
            self.reset();
            self.total = total;
            self.next_index = 1;
        }
        if self.complete || total != self.total || index != self.next_index {
            return Err(());
        }
        self.body.extend_from_slice(&frame[2..]);
        if self.body.len() > 65_535 {
            return Err(());
        }
        self.next_index = index.saturating_add(1);
        if index != total {
            return Ok(None);
        }
        self.complete = true;
        Ok(Some(self.body.clone()))
    }
}

impl Drop for BeeSimResponseDecoder {
    fn drop(&mut self) {
        self.body.zeroize();
    }
}

#[derive(Default)]
struct CcidResponseDecoder {
    frame: Vec<u8>,
    expected_length: Option<usize>,
}

impl CcidResponseDecoder {
    fn reset(&mut self) {
        self.frame.zeroize();
        self.frame.clear();
        self.expected_length = None;
    }

    fn add(&mut self, data: &[u8]) -> Result<Option<Vec<u8>>, CcidDecodeError> {
        if self.frame.len().saturating_add(data.len()) > 65_545 {
            self.reset();
            return Err(CcidDecodeError::Malformed);
        }
        self.frame.extend_from_slice(data);
        if self.expected_length.is_none() {
            if self.frame.len() < 10 {
                return Ok(None);
            }
            let declared = u32::from_le_bytes(
                self.frame[1..5]
                    .try_into()
                    .map_err(|_| CcidDecodeError::Malformed)?,
            );
            if self.frame[7] & 0x80 != 0 && declared == 0 {
                self.reset();
                return Err(CcidDecodeError::TimeExtension);
            }
            if declared > 65_535 {
                self.reset();
                return Err(CcidDecodeError::Malformed);
            }
            self.expected_length = Some(10 + declared as usize);
        }
        let expected = self.expected_length.ok_or(CcidDecodeError::Malformed)?;
        if self.frame.len() > expected {
            self.reset();
            return Err(CcidDecodeError::Malformed);
        }
        if self.frame.len() < expected {
            return Ok(None);
        }
        let result = self.frame.clone();
        self.reset();
        Ok(Some(result))
    }
}

impl Drop for CcidResponseDecoder {
    fn drop(&mut self) {
        self.frame.zeroize();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CcidDecodeError {
    Malformed,
    TimeExtension,
}

#[derive(Default)]
struct LegacyResponseDecoder {
    bytes: Vec<u8>,
    expected_length: Option<usize>,
}

impl LegacyResponseDecoder {
    fn reset(&mut self) {
        self.bytes.zeroize();
        self.bytes.clear();
        self.expected_length = None;
    }

    fn add(&mut self, data: &[u8]) -> Result<Option<Vec<u8>>, ()> {
        self.bytes.extend_from_slice(data);
        if self.expected_length.is_none() {
            if self.bytes.len() < 3 {
                return Ok(None);
            }
            self.expected_length =
                Some(usize::from(self.bytes[1]) | (usize::from(self.bytes[2]) << 8));
            self.bytes.drain(..3);
        }
        let expected = self.expected_length.ok_or(())?;
        if self.bytes.len() > expected {
            self.reset();
            return Err(());
        }
        if self.bytes.len() < expected {
            return Ok(None);
        }
        let result = self.bytes.clone();
        self.reset();
        Ok(Some(result))
    }
}

impl Drop for LegacyResponseDecoder {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecodeError {
    Malformed,
    Stale,
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum HandshakeStage {
    Idle = 0,
    WaitingChallenge = 1,
    WritingResponse = 2,
    WaitingAcknowledgement = 3,
    Authenticated = 4,
}

struct NbleHandshake {
    stage: HandshakeStage,
    next_counter: u8,
}

impl Default for NbleHandshake {
    fn default() -> Self {
        Self {
            stage: HandshakeStage::Idle,
            next_counter: 0,
        }
    }
}

impl NbleHandshake {
    fn reset(&mut self) {
        self.stage = HandshakeStage::Idle;
        self.next_counter = 0;
    }

    fn start(&mut self) {
        self.stage = HandshakeStage::WaitingChallenge;
        self.next_counter = 0;
    }

    fn response_for_challenge(&mut self, challenge: &[u8], key: &[u8]) -> Option<[u8; 17]> {
        if self.stage != HandshakeStage::WaitingChallenge {
            return None;
        }
        let response = challenge_response(challenge, key)?;
        self.stage = HandshakeStage::WritingResponse;
        Some(response)
    }

    fn response_write_completed(&mut self) -> bool {
        if self.stage != HandshakeStage::WritingResponse {
            return false;
        }
        self.stage = HandshakeStage::WaitingAcknowledgement;
        true
    }

    fn classify_acknowledgement(&mut self, data: &[u8]) -> i8 {
        if data.len() != 3 || data[0] != 0xe0 {
            return 0;
        }
        if self.stage == HandshakeStage::WritingResponse {
            return 2;
        }
        if self.stage != HandshakeStage::WaitingAcknowledgement {
            return 0;
        }
        if data[1] == 0x90 && data[2] == 0 {
            self.stage = HandshakeStage::Authenticated;
            1
        } else {
            self.stage = HandshakeStage::Idle;
            -4
        }
    }

    fn take_counter(&mut self) -> u8 {
        let counter = self.next_counter;
        self.next_counter = self.next_counter.wrapping_add(1);
        counter
    }

    fn expected_response_counter(&self) -> u8 {
        self.next_counter.wrapping_sub(1)
    }
}

fn encode_beesim(data: &[u8], mtu: usize) -> Option<Vec<u8>> {
    let frame_payload = mtu.min(244).saturating_sub(2).max(18);
    let total = if data.is_empty() {
        1
    } else {
        data.len().div_ceil(frame_payload)
    };
    if total > usize::from(u8::MAX) {
        return None;
    }
    let mut encoded = Vec::with_capacity(data.len() + total * 2);
    for index in 0..total {
        let start = index * frame_payload;
        let end = (start + frame_payload).min(data.len());
        encoded.extend_from_slice(&[total as u8, (index + 1) as u8]);
        encoded.extend_from_slice(&data[start..end]);
    }
    Some(encoded)
}

fn encode_ccid_apdu(apdu: &[u8]) -> Option<Vec<u8>> {
    if apdu.len() > 65_535 {
        return None;
    }
    let mut frame = Vec::with_capacity(10 + apdu.len());
    frame.push(0x6f);
    frame.extend_from_slice(&(apdu.len() as u32).to_le_bytes());
    frame.extend_from_slice(&[0, 0, 0, 0, 0]);
    frame.extend_from_slice(apdu);
    Some(frame)
}

fn ccid_apdu_response(frame: &[u8]) -> Option<Vec<u8>> {
    if frame.len() < 10 {
        return None;
    }
    let payload_length = u32::from_le_bytes(frame[1..5].try_into().ok()?) as usize;
    if payload_length > 65_535 || frame.len() < 10 + payload_length {
        return None;
    }
    Some(frame[10..10 + payload_length].to_vec())
}

fn ccid_claim_continuation(frame: &[u8]) -> Option<Vec<u8>> {
    if frame.len() < 10 {
        return None;
    }
    let payload_length = u32::from_le_bytes(frame[1..5].try_into().ok()?) as usize;
    if payload_length < 2 || frame.len() < 10 + payload_length || frame[10] != 0x61 {
        return None;
    }
    Some(vec![
        0x6f, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 1, frame[11],
    ])
}

fn encode_legacy_apdu(apdu: &[u8]) -> Option<Vec<u8>> {
    if apdu.len() > 65_535 {
        return None;
    }
    let mut frame = Vec::with_capacity(3 + apdu.len());
    frame.extend_from_slice(&[4, apdu.len() as u8, (apdu.len() >> 8) as u8]);
    frame.extend_from_slice(apdu);
    Some(frame)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_device_suffix(
    name: *const u8,
    name_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(name) = input_slice(name, name_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    let Some(value) = suffix(name) else {
        return FFI_MALFORMED;
    };
    if output.len() < value.len() {
        return FFI_CAPACITY;
    }
    output[..value.len()].copy_from_slice(&value);
    value.len() as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_resolve_password(
    name: *const u8,
    name_length: usize,
    password: *const u8,
    password_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(name) = input_slice(name, name_length) else {
        return FFI_MALFORMED;
    };
    let Some(password) = input_slice(password, password_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    let Some(resolved) = normalize_password(name, password) else {
        return FFI_MALFORMED;
    };
    if output.len() < resolved.len() {
        return FFI_CAPACITY;
    }
    output[..resolved.len()].copy_from_slice(&resolved);
    resolved.len() as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_derive_key(
    name: *const u8,
    name_length: usize,
    password: *const u8,
    password_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(name) = input_slice(name, name_length) else {
        return FFI_MALFORMED;
    };
    let Some(password) = input_slice(password, password_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    let Some(key) = derive_key(name, password) else {
        return FFI_MALFORMED;
    };
    if output.len() < key.len() {
        return FFI_CAPACITY;
    }
    output[..key.len()].copy_from_slice(key.as_ref());
    key.len() as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_challenge_response(
    challenge: *const u8,
    challenge_length: usize,
    key: *const u8,
    key_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(challenge) = input_slice(challenge, challenge_length) else {
        return FFI_MALFORMED;
    };
    let Some(key) = input_slice(key, key_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    let Some(response) = challenge_response(challenge, key) else {
        return FFI_MALFORMED;
    };
    if output.len() < response.len() {
        return FFI_CAPACITY;
    }
    output[..response.len()].copy_from_slice(&response);
    response.len() as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_checksum(bytes: *const u8, length: usize) -> u16 {
    let Some(bytes) = input_slice(bytes, length) else {
        return 0;
    };
    checksum(bytes)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_encode(
    counter: u8,
    apdu: *const u8,
    apdu_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(apdu) = input_slice(apdu, apdu_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    let Some(frames) = encode_nble(counter, apdu) else {
        return FFI_MALFORMED;
    };
    let required = frames.iter().map(|frame| frame.len() + 2).sum::<usize>();
    if output.len() < required {
        return FFI_CAPACITY;
    }
    let mut offset = 0;
    for frame in frames {
        let length = frame.len() as u16;
        output[offset..offset + 2].copy_from_slice(&length.to_be_bytes());
        offset += 2;
        output[offset..offset + frame.len()].copy_from_slice(&frame);
        offset += frame.len();
    }
    offset as isize
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_nble_decoder_create() -> *mut c_void {
    Box::into_raw(Box::<NbleResponseDecoder>::default()).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_decoder_reset(handle: *mut c_void) {
    if let Some(decoder) = unsafe { handle.cast::<NbleResponseDecoder>().as_mut() } {
        decoder.reset();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_decoder_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: `handle` must be a pointer returned by this module's create function.
        drop(unsafe { Box::from_raw(handle.cast::<NbleResponseDecoder>()) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_decoder_add(
    handle: *mut c_void,
    frame: *const u8,
    frame_length: usize,
    expected_counter: u8,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(decoder) = (unsafe { handle.cast::<NbleResponseDecoder>().as_mut() }) else {
        return FFI_MALFORMED;
    };
    let Some(frame) = input_slice(frame, frame_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    match decoder.add(frame, expected_counter) {
        Ok(None) => 0,
        Ok(Some(response)) => {
            if output.len() < response.len() {
                return FFI_CAPACITY;
            }
            output[..response.len()].copy_from_slice(&response);
            response.len() as isize
        }
        Err(DecodeError::Malformed) => FFI_MALFORMED,
        Err(DecodeError::Stale) => FFI_STALE,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_nble_handshake_create() -> *mut c_void {
    Box::into_raw(Box::<NbleHandshake>::default()).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_handshake_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: `handle` must be a pointer returned by this module's create function.
        drop(unsafe { Box::from_raw(handle.cast::<NbleHandshake>()) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_handshake_reset(handle: *mut c_void) {
    if let Some(handshake) = unsafe { handle.cast::<NbleHandshake>().as_mut() } {
        handshake.reset();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_handshake_start(handle: *mut c_void) -> u8 {
    let Some(handshake) = (unsafe { handle.cast::<NbleHandshake>().as_mut() }) else {
        return 0;
    };
    handshake.start();
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_handshake_stage(handle: *mut c_void) -> u8 {
    let Some(handshake) = (unsafe { handle.cast::<NbleHandshake>().as_ref() }) else {
        return 0;
    };
    handshake.stage as u8
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_handshake_next_counter(handle: *mut c_void) -> u8 {
    let Some(handshake) = (unsafe { handle.cast::<NbleHandshake>().as_mut() }) else {
        return 0;
    };
    handshake.take_counter()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_handshake_expected_response_counter(
    handle: *mut c_void,
) -> u8 {
    let Some(handshake) = (unsafe { handle.cast::<NbleHandshake>().as_ref() }) else {
        return 0;
    };
    handshake.expected_response_counter()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_handshake_challenge(
    handle: *mut c_void,
    challenge: *const u8,
    challenge_length: usize,
    key: *const u8,
    key_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(handshake) = (unsafe { handle.cast::<NbleHandshake>().as_mut() }) else {
        return FFI_MALFORMED;
    };
    let Some(challenge) = input_slice(challenge, challenge_length) else {
        return FFI_MALFORMED;
    };
    let Some(key) = input_slice(key, key_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    let Some(response) = handshake.response_for_challenge(challenge, key) else {
        return FFI_MALFORMED;
    };
    if output.len() < response.len() {
        return FFI_CAPACITY;
    }
    output[..response.len()].copy_from_slice(&response);
    response.len() as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_handshake_write_completed(handle: *mut c_void) -> u8 {
    let Some(handshake) = (unsafe { handle.cast::<NbleHandshake>().as_mut() }) else {
        return 0;
    };
    u8::from(handshake.response_write_completed())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_nble_handshake_acknowledgement(
    handle: *mut c_void,
    data: *const u8,
    data_length: usize,
) -> i8 {
    let Some(handshake) = (unsafe { handle.cast::<NbleHandshake>().as_mut() }) else {
        return -1;
    };
    let Some(data) = input_slice(data, data_length) else {
        return -1;
    };
    handshake.classify_acknowledgement(data)
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_nble_keepalive_due(idle_milliseconds: u64, request_pending: u8) -> u8 {
    u8::from(request_pending == 0 && idle_milliseconds >= 10_000)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_beesim_encode(
    data: *const u8,
    data_length: usize,
    mtu: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(data) = input_slice(data, data_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    let Some(encoded) = encode_beesim(data, mtu) else {
        return FFI_MALFORMED;
    };
    if output.len() < encoded.len() {
        return FFI_CAPACITY;
    }
    output[..encoded.len()].copy_from_slice(&encoded);
    encoded.len() as isize
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_beesim_decoder_create() -> *mut c_void {
    Box::into_raw(Box::<BeeSimResponseDecoder>::default()).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_beesim_decoder_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: `handle` must be a pointer returned by this module's create function.
        drop(unsafe { Box::from_raw(handle.cast::<BeeSimResponseDecoder>()) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_beesim_decoder_reset(handle: *mut c_void) {
    if let Some(decoder) = unsafe { handle.cast::<BeeSimResponseDecoder>().as_mut() } {
        decoder.reset();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_beesim_decoder_add(
    handle: *mut c_void,
    frame: *const u8,
    frame_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(decoder) = (unsafe { handle.cast::<BeeSimResponseDecoder>().as_mut() }) else {
        return FFI_MALFORMED;
    };
    let Some(frame) = input_slice(frame, frame_length) else {
        return FFI_MALFORMED;
    };
    match decoder.add(frame) {
        Ok(None) => 0,
        Ok(Some(response)) => {
            let Some(output) = output_slice(output, output_capacity) else {
                return FFI_MALFORMED;
            };
            if output.len() < response.len() {
                return FFI_CAPACITY;
            }
            output[..response.len()].copy_from_slice(&response);
            response.len() as isize + 1
        }
        Err(()) => FFI_MALFORMED,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_ccid_encode_apdu(
    apdu: *const u8,
    apdu_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(apdu) = input_slice(apdu, apdu_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    let Some(encoded) = encode_ccid_apdu(apdu) else {
        return FFI_MALFORMED;
    };
    if output.len() < encoded.len() {
        return FFI_CAPACITY;
    }
    output[..encoded.len()].copy_from_slice(&encoded);
    encoded.len() as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_ccid_extract_apdu(
    frame: *const u8,
    frame_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(frame) = input_slice(frame, frame_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    let Some(apdu) = ccid_apdu_response(frame) else {
        return FFI_MALFORMED;
    };
    if output.len() < apdu.len() {
        return FFI_CAPACITY;
    }
    output[..apdu.len()].copy_from_slice(&apdu);
    apdu.len() as isize + 1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_ccid_claim_continuation(
    frame: *const u8,
    frame_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(frame) = input_slice(frame, frame_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    let Some(continuation) = ccid_claim_continuation(frame) else {
        return 0;
    };
    if output.len() < continuation.len() {
        return FFI_CAPACITY;
    }
    output[..continuation.len()].copy_from_slice(&continuation);
    continuation.len() as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_legacy_encode_apdu(
    apdu: *const u8,
    apdu_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(apdu) = input_slice(apdu, apdu_length) else {
        return FFI_MALFORMED;
    };
    let Some(output) = output_slice(output, output_capacity) else {
        return FFI_MALFORMED;
    };
    let Some(encoded) = encode_legacy_apdu(apdu) else {
        return FFI_MALFORMED;
    };
    if output.len() < encoded.len() {
        return FFI_CAPACITY;
    }
    output[..encoded.len()].copy_from_slice(&encoded);
    encoded.len() as isize
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_ccid_decoder_create() -> *mut c_void {
    Box::into_raw(Box::<CcidResponseDecoder>::default()).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_ccid_decoder_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: `handle` must be a pointer returned by this module's create function.
        drop(unsafe { Box::from_raw(handle.cast::<CcidResponseDecoder>()) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_ccid_decoder_reset(handle: *mut c_void) {
    if let Some(decoder) = unsafe { handle.cast::<CcidResponseDecoder>().as_mut() } {
        decoder.reset();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_ccid_decoder_add(
    handle: *mut c_void,
    data: *const u8,
    data_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(decoder) = (unsafe { handle.cast::<CcidResponseDecoder>().as_mut() }) else {
        return FFI_MALFORMED;
    };
    let Some(data) = input_slice(data, data_length) else {
        return FFI_MALFORMED;
    };
    match decoder.add(data) {
        Ok(None) => 0,
        Ok(Some(frame)) => {
            let Some(output) = output_slice(output, output_capacity) else {
                return FFI_MALFORMED;
            };
            if output.len() < frame.len() {
                return FFI_CAPACITY;
            }
            output[..frame.len()].copy_from_slice(&frame);
            frame.len() as isize + 1
        }
        Err(CcidDecodeError::TimeExtension) => FFI_TIME_EXTENSION,
        Err(CcidDecodeError::Malformed) => FFI_MALFORMED,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_legacy_decoder_create() -> *mut c_void {
    Box::into_raw(Box::<LegacyResponseDecoder>::default()).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_legacy_decoder_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: `handle` must be a pointer returned by this module's create function.
        drop(unsafe { Box::from_raw(handle.cast::<LegacyResponseDecoder>()) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_legacy_decoder_reset(handle: *mut c_void) {
    if let Some(decoder) = unsafe { handle.cast::<LegacyResponseDecoder>().as_mut() } {
        decoder.reset();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_legacy_decoder_add(
    handle: *mut c_void,
    data: *const u8,
    data_length: usize,
    output: *mut u8,
    output_capacity: usize,
) -> isize {
    let Some(decoder) = (unsafe { handle.cast::<LegacyResponseDecoder>().as_mut() }) else {
        return FFI_MALFORMED;
    };
    let Some(data) = input_slice(data, data_length) else {
        return FFI_MALFORMED;
    };
    match decoder.add(data) {
        Ok(None) => 0,
        Ok(Some(response)) => {
            let Some(output) = output_slice(output, output_capacity) else {
                return FFI_MALFORMED;
            };
            if output.len() < response.len() {
                return FFI_CAPACITY;
            }
            output[..response.len()].copy_from_slice(&response);
            response.len() as isize + 1
        }
        Err(()) => FFI_MALFORMED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_protocol_selection_preserves_name_and_nus_priority() {
        let cases = [
            ("ESTKme-RED", false, ReaderProtocol::Ccid),
            ("estkme_red", true, ReaderProtocol::Ccid),
            ("BeeSIM", false, ReaderProtocol::BeeSim),
            ("bee_sim", true, ReaderProtocol::BeeSim),
            ("ESTKme BeeSIM 9eSIM", true, ReaderProtocol::Ccid),
            ("BeeSIM 9eSIM", true, ReaderProtocol::BeeSim),
            ("9eSIM", false, ReaderProtocol::BeeSim),
            ("9ESIM", true, ReaderProtocol::NineEsim),
            ("unrecognized", true, ReaderProtocol::NineEsim),
            ("unrecognized", false, ReaderProtocol::Legacy),
            ("Bee-SIM", false, ReaderProtocol::Legacy),
            ("", false, ReaderProtocol::Legacy),
            ("", true, ReaderProtocol::NineEsim),
            ("İESTKME", false, ReaderProtocol::Ccid),
            ("ESTKME", false, ReaderProtocol::Ccid),
        ];
        for (name, nus, expected) in cases {
            assert_eq!(
                detect_reader_protocol(name, nus),
                expected,
                "{name:?} nus={nus}"
            );
            assert_eq!(
                unsafe { switch_ble_detect_protocol(name.as_ptr(), name.len(), u8::from(nus)) },
                expected as u8,
                "FFI: {name:?} nus={nus}"
            );
        }
    }

    #[test]
    fn reader_protocol_selection_rejects_invalid_utf8_and_flags() {
        let invalid = [0xff, b'E', b'S', b'T', b'K', b'M', b'E'];
        assert_eq!(
            unsafe { switch_ble_detect_protocol(invalid.as_ptr(), invalid.len(), 1) },
            ReaderProtocol::Legacy as u8
        );
        assert_eq!(
            unsafe { switch_ble_detect_protocol(std::ptr::null(), 7, 1) },
            ReaderProtocol::Legacy as u8
        );
        assert_eq!(
            unsafe { switch_ble_detect_protocol(b"9eSIM".as_ptr(), 5, 2) },
            ReaderProtocol::Legacy as u8
        );
        assert_eq!(
            unsafe { switch_ble_detect_protocol(std::ptr::null(), 0, 1) },
            ReaderProtocol::NineEsim as u8
        );
    }

    fn hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn b1_credential_derivation_and_challenge_match_reviewed_vectors() {
        assert_eq!(default_password(b"9eSIMABCDEF"), Some(*b"fedcba"));
        assert_eq!(suffix(b" 9eSIMAbCdEf \n"), Some(*b"abcdef"));
        let challenge_key = derive_key(b"9eSIMABCDEF", b"fedcba").unwrap();
        assert_eq!(
            challenge_key.as_ref(),
            &hex("effedcbaeffedcbaeffedcba00000000")[..]
        );
        let crc_key = derive_key(b"9eSIM123456", b"123456789").unwrap();
        assert_eq!(
            crc_key.as_ref(),
            &hex("5621cf025621cf025621cf0200000000")[..]
        );
        let challenge = hex("e078d5ae112d1ae7b14bb482f9769d6362");
        assert_eq!(
            challenge_response(&challenge, challenge_key.as_ref()).unwrap(),
            hex("e00440880ef37d23924bb482f9769d6362").as_slice()
        );
    }

    #[test]
    fn nble_frames_checksum_sequence_wrap_and_decoder_errors_match_contract() {
        let frames = encode_nble(0, &[0xfe, 0x0d, 0, 0, 1]).unwrap();
        assert_eq!(frames, vec![hex("2000fe0d000001010c")]);
        let long: Vec<_> = (0..3_400).map(|n| n as u8).collect();
        let frames = encode_nble(0, &long).unwrap();
        assert_eq!(frames.len(), 35);
        assert_eq!(frames[31][0], 0x1f);
        assert_eq!(frames[32][0], 0);
        assert_eq!(frames.last().unwrap()[0], 0x22);

        let body = [5, 0x90, 0];
        let mut response = vec![5, 0x90, 0];
        response.extend_from_slice(&checksum(&body).to_be_bytes());
        let frame = [&[0x20][..], &response].concat();
        let mut stale = NbleResponseDecoder::default();
        assert_eq!(stale.add(&frame, 6), Err(DecodeError::Stale));
        stale.reset();
        let mut valid = NbleResponseDecoder::default();
        assert_eq!(valid.add(&frame, 5).unwrap(), Some(vec![0x90, 0]));
        let mut malformed = NbleResponseDecoder::default();
        let mut corrupt = frame;
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(malformed.add(&corrupt, 5), Err(DecodeError::Malformed));
    }

    #[test]
    fn beesim_frame_limits_and_empty_payload_preserve_wire_shape() {
        assert_eq!(encode_beesim(&[], 20), Some(vec![1, 1]));
        assert_eq!(encode_beesim(&[1, 2, 3], 20), Some(vec![1, 1, 1, 2, 3]));
        let large = vec![0xa5; 255 * 18];
        assert!(encode_beesim(&large, 20).is_some());
        let oversized = vec![0xa5; 255 * 18 + 1];
        assert!(encode_beesim(&oversized, 20).is_none());

        let mut decoder = BeeSimResponseDecoder::default();
        assert_eq!(decoder.add(&[2, 1, 0x90]), Ok(None));
        assert_eq!(decoder.add(&[2, 2, 0]), Ok(Some(vec![0x90, 0])));
        assert!(decoder.add(&[2, 2, 0]).is_err());
        decoder.reset();
        assert!(decoder.add(&[3, 2, 0x90]).is_err());
        assert_eq!(decoder.add(&[1, 1, 0x90, 0]), Ok(Some(vec![0x90, 0])));
        decoder.reset();
        assert_eq!(decoder.add(&[1, 1]), Ok(Some(Vec::new())));
    }

    #[test]
    fn handshake_stage_and_keepalive_policy_are_owned_by_rust() {
        let key = derive_key(b"9eSIMABCDEF", b"fedcba").unwrap();
        let challenge = hex("e078d5ae112d1ae7b14bb482f9769d6362");
        let mut state = NbleHandshake::default();
        assert_eq!(state.stage as u8, HandshakeStage::Idle as u8);
        state.start();
        assert_eq!(state.stage as u8, HandshakeStage::WaitingChallenge as u8);
        assert_eq!(state.take_counter(), 0);
        assert_eq!(state.expected_response_counter(), 0);
        assert_eq!(state.take_counter(), 1);
        assert_eq!(state.expected_response_counter(), 1);
        state.next_counter = u8::MAX;
        assert_eq!(state.take_counter(), u8::MAX);
        assert_eq!(state.expected_response_counter(), u8::MAX);
        assert!(
            state
                .response_for_challenge(&challenge, key.as_ref())
                .is_some()
        );
        assert_eq!(state.stage as u8, HandshakeStage::WritingResponse as u8);
        assert_eq!(state.classify_acknowledgement(&[0xe0, 0x90, 0]), 2);
        assert!(state.response_write_completed());
        assert_eq!(state.classify_acknowledgement(&[0xe0, 0x90, 0]), 1);
        assert_eq!(state.stage as u8, HandshakeStage::Authenticated as u8);

        let mut rejected = NbleHandshake::default();
        rejected.start();
        assert!(
            rejected
                .response_for_challenge(&challenge, key.as_ref())
                .is_some()
        );
        assert!(rejected.response_write_completed());
        assert_eq!(rejected.classify_acknowledgement(&[0xe0, 0x69, 0x82]), -4);
        assert_eq!(rejected.stage as u8, HandshakeStage::Idle as u8);

        assert_eq!(switch_ble_nble_keepalive_due(9_999, 0), 0);
        assert_eq!(switch_ble_nble_keepalive_due(10_000, 0), 1);
        assert_eq!(switch_ble_nble_keepalive_due(60_000, 1), 0);
    }

    #[test]
    fn ccid_and_legacy_frames_preserve_lengths_time_extensions_and_payloads() {
        assert_eq!(
            encode_ccid_apdu(&[0x00, 0xa4, 0x04, 0x00]).unwrap(),
            vec![0x6f, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xa4, 0x04, 0]
        );
        assert_eq!(encode_legacy_apdu(&[1, 2, 3]), Some(vec![4, 3, 0, 1, 2, 3]));

        let mut response = vec![0x80, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0x90, 0];
        assert_eq!(ccid_apdu_response(&response), Some(vec![0x90, 0]));
        let continuation = ccid_claim_continuation(&[0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0x61, 0x10]);
        assert_eq!(
            continuation,
            Some(vec![
                0x6f, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 1, 0x10
            ])
        );

        let mut ccid = CcidResponseDecoder::default();
        assert_eq!(ccid.add(&response[..5]), Ok(None));
        assert_eq!(ccid.add(&response[5..]), Ok(Some(response.clone())));
        let extension = [0x80, 0, 0, 0, 0, 0, 0, 0x80, 0, 0];
        assert_eq!(ccid.add(&extension), Err(CcidDecodeError::TimeExtension));
        assert_eq!(ccid.add(&vec![0; 65_546]), Err(CcidDecodeError::Malformed));
        response[1] = 3;
        assert!(ccid_apdu_response(&response).is_none());

        let mut legacy = LegacyResponseDecoder::default();
        assert_eq!(legacy.add(&[4, 2]), Ok(None));
        assert_eq!(legacy.add(&[0, 0x90]), Ok(None));
        assert_eq!(legacy.add(&[0]), Ok(Some(vec![0x90, 0])));
        assert_eq!(legacy.add(&[4, 0, 0]), Ok(Some(Vec::new())));
    }
}
