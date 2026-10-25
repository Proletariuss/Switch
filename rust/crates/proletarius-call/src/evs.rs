//! Safe, per-call access to the pinned 3GPP EVS reference codec.
//!
//! The unsafe code is limited to this FFI adapter. Each instance owns one C
//! encoder/decoder state; RTP framing and codec negotiation stay in Rust.

use std::{ffi::c_void, fmt, ptr::NonNull};

/// Explicit debug-only native fault-test build; absent from shipping builds.
#[cfg(feature = "native-codec-fault-tests")]
pub mod fault_test_support;

const SAMPLES_PER_FRAME: usize = 320;
const MAX_ENCODED_BITS: usize = 480;

#[allow(unsafe_code)]
unsafe extern "C" {
    fn switch_evs_create(amr_wb_mode: i32) -> *mut c_void;
    fn switch_evs_destroy(state: *mut c_void);
    fn switch_evs_set_amr_wb_mode(state: *mut c_void, mode: i32) -> i32;
    fn switch_evs_encode(state: *mut c_void, pcm: *const i16, bits: *mut u8) -> i32;
    fn switch_evs_decode(
        state: *mut c_void,
        bits: *const u8,
        bit_count: i32,
        mode: i32,
        lost: i32,
        pcm: *mut i16,
    ) -> i32;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodecProfile {
    EvsPrimaryWb132,
    AmrWb { mode: u8 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodecError {
    Unavailable,
    InvalidAmrWbMode(u8),
    AmrWbModeChangeUnsupported,
    AmrWbModeChangeFailed,
    InvalidFrameMode(u8),
    InvalidFrameLength { expected: usize, actual: usize },
    EncodedFrameTooLarge,
    EncodeFailed,
    DecodeFailed,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("EVS reference codec is unavailable"),
            Self::InvalidAmrWbMode(mode) => write!(f, "unsupported AMR-WB mode {mode}"),
            Self::AmrWbModeChangeUnsupported => {
                f.write_str("AMR-WB mode changes require an AMR-WB codec profile")
            }
            Self::AmrWbModeChangeFailed => f.write_str("AMR-WB encoder mode change failed"),
            Self::InvalidFrameMode(mode) => write!(f, "unsupported codec frame mode {mode}"),
            Self::InvalidFrameLength { expected, actual } => {
                write!(f, "codec frame needs {expected} bytes, got {actual}")
            }
            Self::EncodedFrameTooLarge => f.write_str("codec frame exceeds the output bound"),
            Self::EncodeFailed => f.write_str("codec encoder failed"),
            Self::DecodeFailed => f.write_str("codec decoder failed"),
        }
    }
}

impl std::error::Error for CodecError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadError {
    Empty,
    UnsupportedToc,
    UnsupportedMode(u8),
    InvalidFrameLength { expected: usize, actual: usize },
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty EVS RTP payload"),
            Self::UnsupportedToc => f.write_str("unsupported EVS ToC"),
            Self::UnsupportedMode(mode) => write!(f, "unsupported EVS frame mode {mode}"),
            Self::InvalidFrameLength { expected, actual } => {
                write!(f, "EVS frame needs {expected} bytes, got {actual}")
            }
        }
    }
}

impl std::error::Error for PayloadError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrimaryFrame<'a> {
    pub bits: &'a [u8],
    pub mode: u8,
}

/// Packetize one EVS Primary frame. The current negotiated profile uses mode 4
/// (WB 13.2 kbps); full_header selects the optional one-byte EVS ToC form.
pub fn packetize_primary(
    bits: &[u8],
    mode: u8,
    full_header: bool,
) -> Result<Vec<u8>, PayloadError> {
    let expected = primary_frame_bits(mode)
        .ok_or(PayloadError::UnsupportedMode(mode))?
        .div_ceil(8);
    if bits.len() != expected {
        return Err(PayloadError::InvalidFrameLength {
            expected,
            actual: bits.len(),
        });
    }
    let mut payload = Vec::with_capacity(bits.len() + usize::from(full_header));
    if full_header {
        payload.push(mode & 0x0f);
    }
    payload.extend_from_slice(bits);
    Ok(payload)
}

/// Parse the single-frame Primary EVS formats used by the call path. Compact
/// mode is inferred by exact payload length; full-header mode accepts one
/// optional CMR byte and rejects multi-frame, IO and reserved ToCs.
pub fn depacketize_primary(
    mut payload: &[u8],
    full_header_only: bool,
) -> Result<PrimaryFrame<'_>, PayloadError> {
    if !full_header_only {
        // 56-bit SC-VBR mode is intentionally excluded because its compact
        // length is ambiguous with another EVS framing form.
        for mode in 1..=12 {
            let expected = primary_frame_bits(mode)
                .expect("supported primary mode")
                .div_ceil(8);
            if payload.len() == expected {
                return Ok(PrimaryFrame {
                    bits: payload,
                    mode,
                });
            }
        }
    }

    if payload.is_empty() {
        return Err(PayloadError::Empty);
    }
    if payload[0] & 0x80 != 0 {
        payload = &payload[1..];
    }
    let Some((&toc, bits)) = payload.split_first() else {
        return Err(PayloadError::Empty);
    };
    if toc & 0xf0 != 0 {
        return Err(PayloadError::UnsupportedToc);
    }
    let mode = toc & 0x0f;
    let expected = primary_frame_bits(mode)
        .ok_or(PayloadError::UnsupportedMode(mode))?
        .div_ceil(8);
    if bits.len() != expected {
        return Err(PayloadError::InvalidFrameLength {
            expected,
            actual: bits.len(),
        });
    }
    Ok(PrimaryFrame { bits, mode })
}

/// Stateful 20ms codec instance. It is deliberately not `Clone`, `Send`, or
/// `Sync`; its C state must remain owned by the media worker that created it.
pub struct EvsCodec {
    state: NonNull<c_void>,
    profile: CodecProfile,
}

impl EvsCodec {
    #[allow(unsafe_code)]
    pub fn new(profile: CodecProfile) -> Result<Self, CodecError> {
        let amr_wb_mode = match profile {
            CodecProfile::EvsPrimaryWb132 => -1,
            CodecProfile::AmrWb { mode } if mode <= 8 => i32::from(mode),
            CodecProfile::AmrWb { mode } => return Err(CodecError::InvalidAmrWbMode(mode)),
        };

        #[cfg(switch_evs_available)]
        {
            // SAFETY: the C constructor accepts a validated mode and returns
            // either null or a uniquely owned per-call state.
            let state = unsafe { switch_evs_create(amr_wb_mode) };
            let state = NonNull::new(state).ok_or(CodecError::Unavailable)?;
            Ok(Self { state, profile })
        }
        #[cfg(not(switch_evs_available))]
        {
            let _ = amr_wb_mode;
            let _ = profile;
            Err(CodecError::Unavailable)
        }
    }

    pub fn profile(&self) -> CodecProfile {
        self.profile
    }

    /// Update the next AMR-WB encoding frame without replacing the encoder or
    /// decoder state. The owning media worker calls this between 20ms frames;
    /// negotiated mode-set and CMR policy remain the caller's responsibility.
    #[allow(unsafe_code)]
    pub fn set_amr_wb_mode(&mut self, mode: u8) -> Result<(), CodecError> {
        if mode > 8 {
            return Err(CodecError::InvalidAmrWbMode(mode));
        }
        if !matches!(self.profile, CodecProfile::AmrWb { .. }) {
            return Err(CodecError::AmrWbModeChangeUnsupported);
        }
        // SAFETY: state is live and uniquely borrowed, and mode is validated.
        // The C adapter validates before mutation and preserves both states.
        if unsafe { switch_evs_set_amr_wb_mode(self.state.as_ptr(), i32::from(mode)) } != 1 {
            return Err(CodecError::AmrWbModeChangeFailed);
        }
        self.profile = CodecProfile::AmrWb { mode };
        Ok(())
    }

    #[allow(unsafe_code)]
    pub fn encode(&mut self, pcm: &[i16; SAMPLES_PER_FRAME]) -> Result<Vec<u8>, CodecError> {
        let mut bits = [0_u8; MAX_ENCODED_BITS / 8];
        // SAFETY: the state is live and uniquely borrowed; both buffers have
        // the exact sizes required by the C wrapper for one 20ms frame.
        let count =
            unsafe { switch_evs_encode(self.state.as_ptr(), pcm.as_ptr(), bits.as_mut_ptr()) };
        if !(1..=MAX_ENCODED_BITS as i32).contains(&count) {
            return Err(CodecError::EncodeFailed);
        }
        let byte_count = (count as usize).div_ceil(8);
        if byte_count > bits.len() {
            return Err(CodecError::EncodedFrameTooLarge);
        }
        Ok(bits[..byte_count].to_vec())
    }

    /// Decode one primary or AMR-WB frame, or generate PLC when `lost` is set.
    #[allow(unsafe_code)]
    pub fn decode(
        &mut self,
        bits: &[u8],
        mode: u8,
        lost: bool,
    ) -> Result<[i16; SAMPLES_PER_FRAME], CodecError> {
        let bit_count =
            frame_bit_count(self.profile, mode).ok_or(CodecError::InvalidFrameMode(mode))?;
        let expected = bit_count.div_ceil(8);
        if (lost && !bits.is_empty()) || (!lost && bits.len() != expected) {
            return Err(CodecError::InvalidFrameLength {
                expected: if lost { 0 } else { expected },
                actual: bits.len(),
            });
        }
        let mut pcm = [0_i16; SAMPLES_PER_FRAME];
        let bit_ptr = if bits.is_empty() {
            std::ptr::null()
        } else {
            bits.as_ptr()
        };
        // SAFETY: the state is live and uniquely borrowed. The input slice is
        // checked against the selected frame size; the output is 320 samples.
        let decoded = unsafe {
            switch_evs_decode(
                self.state.as_ptr(),
                bit_ptr,
                if lost { 0 } else { bit_count as i32 },
                i32::from(mode),
                i32::from(lost),
                pcm.as_mut_ptr(),
            )
        };
        if decoded != SAMPLES_PER_FRAME as i32 {
            return Err(CodecError::DecodeFailed);
        }
        Ok(pcm)
    }
}

/// Probe the codec implementations linked into this Rust process. Call
/// negotiation consumes these facts instead of trusting a Go-side capability
/// list; each temporary C state is dropped before returning.
pub fn available_audio_codecs(evs_only: bool) -> (bool, Vec<u8>) {
    let evs_available = EvsCodec::new(CodecProfile::EvsPrimaryWb132).is_ok();
    let amr_wb_modes = if evs_only {
        Vec::new()
    } else {
        (0..=8)
            .filter(|mode| EvsCodec::new(CodecProfile::AmrWb { mode: *mode }).is_ok())
            .collect()
    };
    (evs_available, amr_wb_modes)
}

impl Drop for EvsCodec {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: this instance is the unique owner returned by the matching
        // constructor and Drop runs exactly once.
        unsafe { switch_evs_destroy(self.state.as_ptr()) };
    }
}

fn frame_bit_count(profile: CodecProfile, mode: u8) -> Option<usize> {
    match profile {
        CodecProfile::EvsPrimaryWb132 => primary_frame_bits(mode),
        CodecProfile::AmrWb { .. } => crate::amr_wb::frame_bits(mode),
    }
}

fn primary_frame_bits(mode: u8) -> Option<usize> {
    [
        Some(56),
        Some(144),
        Some(160),
        Some(192),
        Some(264),
        Some(328),
        Some(488),
        Some(640),
        Some(960),
        Some(1280),
        Some(1920),
        Some(2560),
        Some(48),
        None,
        Some(0),
        Some(0),
    ]
    .get(usize::from(mode))
    .copied()
    .flatten()
}

// The shipping target must run the native golden tests, rather than silently
// omit them when the codec archive was not built or its path was lost.
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
#[test]
fn shipping_target_has_native_evs_and_all_amr_wb_codecs() {
    let (evs, amr_wb_modes) = available_audio_codecs(false);
    assert!(evs, "build the native archive with scripts/build-evs.py");
    assert_eq!(amr_wb_modes, (0..=8).collect::<Vec<_>>());
}

#[cfg(all(test, switch_evs_available))]
mod tests {
    use super::{
        CodecError, CodecProfile, EvsCodec, PayloadError, PrimaryFrame, depacketize_primary,
        packetize_primary,
    };

    const INPUT_PCM: &[u8] = include_bytes!("../testdata/evs/input.pcm");
    const REFERENCE_EVS: &[u8] = include_bytes!("../testdata/evs/reference.evs");
    const DECODED_PCM: &[u8] = include_bytes!("../testdata/evs/decoded.pcm");
    const AMR_WB_REFERENCE: &[u8] = include_bytes!("../testdata/evs/amrwb1265.evs");
    const AMR_WB_DECODED: &[u8] = include_bytes!("../testdata/evs/amrwb1265.pcm");

    fn input_frame(frame: usize) -> [i16; 320] {
        let mut samples = [0_i16; 320];
        for (sample, bytes) in samples
            .iter_mut()
            .zip(INPUT_PCM[frame * 640..(frame + 1) * 640].chunks_exact(2))
        {
            *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
        }
        samples
    }

    #[test]
    fn amr_wb_1265_matches_independent_encoder_and_decoder_vectors() {
        assert_eq!(INPUT_PCM.len(), 50 * 640);
        assert_eq!(AMR_WB_REFERENCE.len(), 16 + 50 * 33);
        assert_eq!(&AMR_WB_REFERENCE[..16], b"#!EVS_MC1.0\n\0\0\0\x01");
        assert_eq!(AMR_WB_DECODED.len(), 50 * 640);
        // Separate native states keep decoder checks independent of encoding.
        // Both expected outputs came from the unmodified reference CLI.
        let mut encoder = EvsCodec::new(CodecProfile::AmrWb { mode: 2 }).unwrap();
        let mut decoder = EvsCodec::new(CodecProfile::AmrWb { mode: 2 }).unwrap();
        let mut actual_pcm = Vec::with_capacity(AMR_WB_DECODED.len());
        let mut nonzero_samples = 0;
        let mut energy = 0_i64;
        let mut minimum = i16::MAX;
        let mut maximum = i16::MIN;

        for frame in 0..50 {
            let reference = &AMR_WB_REFERENCE[16 + frame * 33..16 + (frame + 1) * 33];
            assert_eq!(reference[0], 0x32, "AMR-WB reference ToC, frame {frame}");
            let encoded = encoder.encode(&input_frame(frame)).unwrap();
            assert_eq!(encoded, reference[1..], "AMR-WB encode, frame {frame}");

            // Decode the frozen bitstream, never the newly encoded frame.
            let decoded = decoder.decode(&reference[1..], 2, false).unwrap();
            for sample in decoded {
                actual_pcm.extend_from_slice(&sample.to_le_bytes());
                nonzero_samples += usize::from(sample != 0);
                energy += i64::from(sample) * i64::from(sample);
                minimum = minimum.min(sample);
                maximum = maximum.max(sample);
            }
            assert_eq!(
                &actual_pcm[frame * 640..(frame + 1) * 640],
                &AMR_WB_DECODED[frame * 640..(frame + 1) * 640],
                "AMR-WB decode, frame {frame}"
            );
        }

        // Frozen reference signal facts also rule out a silent/empty oracle.
        assert_eq!(nonzero_samples, 15_956);
        assert_eq!((minimum, maximum), (-7_286, 7_549));
        assert_eq!(energy, 159_764_719_564);
    }

    #[test]
    fn all_amr_wb_native_modes_support_both_rtp_formats_and_loss_concealment() {
        for mode in 0..=8 {
            let mut codec = EvsCodec::new(CodecProfile::AmrWb { mode }).unwrap();
            for frame in 0..50 {
                let bits = codec.encode(&input_frame(frame)).unwrap();
                assert_eq!(
                    bits.len(),
                    crate::amr_wb::frame_bits(mode).unwrap().div_ceil(8),
                    "AMR-WB mode {mode}, frame {frame}"
                );
                for octet_aligned in [false, true] {
                    let payload = crate::amr_wb::packetize(&bits, mode, octet_aligned).unwrap();
                    let parsed = crate::amr_wb::depacketize(&payload, octet_aligned).unwrap();
                    assert_eq!(parsed.mode, mode);
                    assert!(parsed.good);
                    assert_eq!(parsed.bits, bits);
                }
                assert_eq!(codec.decode(&bits, mode, false).unwrap().len(), 320);
            }
            assert_eq!(codec.decode(&[], 14, true).unwrap().len(), 320);
        }
    }

    #[test]
    fn one_amr_wb_state_encodes_and_decodes_continuously_across_8_2_8() {
        let mut encoder = EvsCodec::new(CodecProfile::AmrWb { mode: 8 }).unwrap();
        let mut decoder = EvsCodec::new(CodecProfile::AmrWb { mode: 8 }).unwrap();
        let encoder_state = encoder.state;
        let decoder_state = decoder.state;
        let mut nonzero_samples = 0;
        for (stage, mode) in [8, 2, 8].into_iter().enumerate() {
            encoder.set_amr_wb_mode(mode).unwrap();
            assert_eq!(encoder.profile(), CodecProfile::AmrWb { mode });
            assert_eq!(encoder.state, encoder_state);
            for frame in stage * 10..(stage + 1) * 10 {
                let bits = encoder.encode(&input_frame(frame)).unwrap();
                assert_eq!(
                    bits.len(),
                    crate::amr_wb::frame_bits(mode).unwrap().div_ceil(8)
                );
                // Decode the changing bitstream through one independent,
                // uninterrupted decoder; the encoder setter must not be
                // required for receiving a different valid AMR-WB FT.
                let pcm = decoder.decode(&bits, mode, false).unwrap();
                // The decoder embedded in the encoder's state sees the same
                // stream. Mode updates must leave it identical to the receiver
                // that never calls the setter.
                assert_eq!(encoder.decode(&bits, mode, false).unwrap(), pcm);
                assert_eq!(pcm.len(), 320);
                assert_eq!(decoder.state, decoder_state);
                nonzero_samples += pcm.iter().filter(|sample| **sample != 0).count();
                for octet_aligned in [false, true] {
                    let packet = crate::amr_wb::packetize(&bits, mode, octet_aligned).unwrap();
                    let parsed = crate::amr_wb::depacketize(&packet, octet_aligned).unwrap();
                    assert_eq!(parsed.mode, mode);
                    assert_eq!(parsed.bits, bits);
                }
            }
        }
        assert!(nonzero_samples > 0);
        assert_eq!(decoder.decode(&[], 14, true).unwrap().len(), 320);
    }

    #[test]
    fn rejected_mode_changes_preserve_profile_and_native_state() {
        for profile in [
            CodecProfile::AmrWb { mode: 8 },
            CodecProfile::EvsPrimaryWb132,
        ] {
            let mut subject = EvsCodec::new(profile).unwrap();
            let mut control = EvsCodec::new(profile).unwrap();
            for frame in 0..10 {
                let pcm = input_frame(frame);
                assert_eq!(subject.encode(&pcm).unwrap(), control.encode(&pcm).unwrap());
            }
            let state = subject.state;
            for mode in 9..=u8::MAX {
                assert_eq!(
                    subject.set_amr_wb_mode(mode),
                    Err(CodecError::InvalidAmrWbMode(mode))
                );
            }
            if profile == CodecProfile::EvsPrimaryWb132 {
                for mode in 0..=8 {
                    assert_eq!(
                        subject.set_amr_wb_mode(mode),
                        Err(CodecError::AmrWbModeChangeUnsupported)
                    );
                }
            }
            assert_eq!(subject.profile(), profile);
            assert_eq!(subject.state, state);
            for frame in 10..20 {
                let pcm = input_frame(frame);
                assert_eq!(subject.encode(&pcm).unwrap(), control.encode(&pcm).unwrap());
            }
        }
    }

    #[test]
    fn matches_independent_reference_encoder_and_decoder_vectors() {
        assert_eq!(REFERENCE_EVS.len(), 16 + 50 * 34);
        assert_eq!(INPUT_PCM.len(), 50 * 640);
        assert_eq!(DECODED_PCM.len(), 50 * 640);
        let mut codec = EvsCodec::new(CodecProfile::EvsPrimaryWb132).unwrap();

        for frame in 0..50 {
            let input = &INPUT_PCM[frame * 640..(frame + 1) * 640];
            let mut samples = [0_i16; 320];
            for (sample, bytes) in samples.iter_mut().zip(input.chunks_exact(2)) {
                *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
            }
            let encoded = codec.encode(&samples).unwrap();
            let reference = &REFERENCE_EVS[16 + frame * 34..16 + (frame + 1) * 34];
            assert_eq!(reference[0], 4, "unexpected reference ToC in frame {frame}");
            assert_eq!(encoded, reference[1..], "encoder mismatch in frame {frame}");

            let decoded = codec.decode(&reference[1..], 4, false).unwrap();
            let output = &DECODED_PCM[frame * 640..(frame + 1) * 640];
            for (sample, bytes) in decoded.iter().zip(output.chunks_exact(2)) {
                assert_eq!(*sample, i16::from_le_bytes([bytes[0], bytes[1]]));
            }
        }

        for _ in 0..8 {
            codec.decode(&[], 14, true).unwrap();
        }
    }

    #[test]
    fn rejects_invalid_frames_and_supports_amr_wb_only_fallback() {
        let mut evs = EvsCodec::new(CodecProfile::EvsPrimaryWb132).unwrap();
        assert_eq!(
            evs.decode(&[0; 32], 4, false).unwrap_err(),
            CodecError::InvalidFrameLength {
                expected: 33,
                actual: 32
            }
        );
        assert!(matches!(
            evs.decode(&[], 13, false),
            Err(CodecError::InvalidFrameMode(13))
        ));

        let mut amr_wb = EvsCodec::new(CodecProfile::AmrWb { mode: 7 }).unwrap();
        let pcm = [0_i16; 320];
        let encoded = amr_wb.encode(&pcm).unwrap();
        assert_eq!(encoded.len(), 58);
        assert_eq!(amr_wb.decode(&encoded, 7, false).unwrap().len(), 320);
        assert!(matches!(
            EvsCodec::new(CodecProfile::AmrWb { mode: 9 }),
            Err(CodecError::InvalidAmrWbMode(9))
        ));
    }

    #[test]
    fn primary_rtp_packet_forms_match_go_single_frame_contract() {
        let bits = vec![0x5a; 33];
        assert_eq!(packetize_primary(&bits, 4, false).unwrap(), bits);
        let full = packetize_primary(&bits, 4, true).unwrap();
        assert_eq!(full[0], 4);
        assert_eq!(
            depacketize_primary(&full, true).unwrap(),
            PrimaryFrame {
                bits: &full[1..],
                mode: 4
            }
        );

        let mut with_cmr = vec![0x80, 4];
        with_cmr.extend_from_slice(&full[1..]);
        assert_eq!(
            depacketize_primary(&with_cmr, true).unwrap(),
            PrimaryFrame {
                bits: &with_cmr[2..],
                mode: 4
            }
        );
        assert_eq!(depacketize_primary(&bits, false).unwrap().mode, 4);

        let mut invalid_toc = vec![0x10];
        invalid_toc.extend_from_slice(&[0; 33]);
        assert!(matches!(
            depacketize_primary(&invalid_toc, true),
            Err(PayloadError::UnsupportedToc)
        ));
        assert!(matches!(
            depacketize_primary(&full[..full.len() - 1], true),
            Err(PayloadError::InvalidFrameLength {
                expected: 33,
                actual: 32
            })
        ));
        assert!(matches!(
            packetize_primary(&bits[..32], 4, true),
            Err(PayloadError::InvalidFrameLength {
                expected: 33,
                actual: 32
            })
        ));
        assert!(matches!(
            depacketize_primary(&[], true),
            Err(PayloadError::Empty)
        ));
    }
}
