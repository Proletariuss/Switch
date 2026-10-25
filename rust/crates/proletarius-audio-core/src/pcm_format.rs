//! PCM format conversion for the CoreAudio device adapter. The adapter owns
//! AudioBufferList lifetimes; the sample and channel policy lives here.

use crate::{f32_to_i16, f32_to_i32, sanitize_f32};

#[derive(Clone, Copy)]
enum PcmFormat {
    Float32,
    Float64,
    Signed16,
    Signed32,
}

impl PcmFormat {
    fn parse(bits: u32, is_float: u8) -> Option<Self> {
        match (bits, is_float) {
            (32, 1) => Some(Self::Float32),
            (64, 1) => Some(Self::Float64),
            (16, 0) => Some(Self::Signed16),
            (32, 0) => Some(Self::Signed32),
            _ => None,
        }
    }

    fn width(self) -> usize {
        match self {
            Self::Signed16 => 2,
            Self::Float32 | Self::Signed32 => 4,
            Self::Float64 => 8,
        }
    }

    fn decode(self, bytes: &[u8]) -> f32 {
        match self {
            Self::Float32 => f32::from_le_bytes(bytes.try_into().expect("format width")),
            Self::Float64 => f64::from_le_bytes(bytes.try_into().expect("format width")) as f32,
            Self::Signed16 => {
                i16::from_le_bytes(bytes.try_into().expect("format width")) as f32 / 32768.0
            }
            Self::Signed32 => {
                i32::from_le_bytes(bytes.try_into().expect("format width")) as f32 / 2147483648.0
            }
        }
    }

    fn encode(self, sample: f32, output: &mut [u8]) {
        let sample = sanitize_f32(sample);
        match self {
            Self::Float32 => output.copy_from_slice(&sample.to_le_bytes()),
            Self::Float64 => output.copy_from_slice(&f64::from(sample).to_le_bytes()),
            Self::Signed16 => output.copy_from_slice(&f32_to_i16(sample).to_le_bytes()),
            Self::Signed32 => output.copy_from_slice(&f32_to_i32(sample).to_le_bytes()),
        }
    }
}

fn frame_width(format: PcmFormat, channels: usize) -> Option<usize> {
    (channels > 0).then_some(())?;
    format.width().checked_mul(channels)
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_pcm_format_supported(bits: u32, is_float: u8) -> u8 {
    u8::from(PcmFormat::parse(bits, is_float).is_some())
}

/// Return the number of complete PCM frames to discard from the front of an
/// input byte queue so the remainder fits the byte limit. Keeping the drop
/// aligned preserves channel boundaries even when the last callback ended in
/// the middle of a frame. Returns -1 for unsupported formats or an impossible
/// limit; the caller must not remove a partial frame in that case.
#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_pcm_front_trim_bytes(
    queued_bytes: usize,
    maximum_bytes: usize,
    bits: u32,
    is_float: u8,
    channels: usize,
) -> isize {
    let Some(format) = PcmFormat::parse(bits, is_float) else {
        return -1;
    };
    let Some(frame_bytes) = frame_width(format, channels) else {
        return -1;
    };
    if maximum_bytes < frame_bytes || queued_bytes > isize::MAX as usize {
        return -1;
    }
    let excess = queued_bytes.saturating_sub(maximum_bytes);
    let frames_to_drop = excess / frame_bytes + usize::from(excess % frame_bytes != 0);
    let Some(drop_bytes) = frames_to_drop.checked_mul(frame_bytes) else {
        return -1;
    };
    if drop_bytes > queued_bytes {
        return -1;
    }
    drop_bytes as isize
}

/// The macOS adapter keeps only the first AudioBuffer for non-interleaved
/// input, then passes one channel here. Interleaved input selects channel 0.
fn decode_first_channel(
    input: &[u8],
    format: PcmFormat,
    channels: usize,
    output: &mut [f32],
) -> Option<usize> {
    let frame_width = frame_width(format, channels)?;
    let frames = input.len() / frame_width;
    if output.len() < frames {
        return None;
    }
    for (sample, frame) in output.iter_mut().zip(input.chunks_exact(frame_width)) {
        *sample = format.decode(&frame[..format.width()]);
    }
    Some(frames)
}

/// Duplicates a mono stream to each AudioBuffer's channels and fills any
/// remaining device frames with silence. Trailing incomplete bytes are intact.
fn encode_mono(
    input: &[f32],
    format: PcmFormat,
    channels: usize,
    output: &mut [u8],
) -> Option<usize> {
    let frame_width = frame_width(format, channels)?;
    let frames = output.len() / frame_width;
    let consumed = input.len().min(frames);
    for (index, frame) in output.chunks_exact_mut(frame_width).enumerate() {
        let sample = input.get(index).copied().unwrap_or(0.0);
        for channel in frame.chunks_exact_mut(format.width()) {
            format.encode(sample, channel);
        }
    }
    Some(consumed)
}

// Validate all address arithmetic before making either Rust slice. This can
// check slice-size, alignment and aliasing requirements, but not allocation
// provenance or whether the caller actually owns the indicated memory.
fn buffer_region<T>(pointer: *const T, length: usize) -> Option<(usize, usize)> {
    if length == 0 {
        return Some((0, 0));
    }
    let bytes = length.checked_mul(std::mem::size_of::<T>())?;
    let start = pointer as usize;
    if pointer.is_null()
        || !start.is_multiple_of(std::mem::align_of::<T>())
        || bytes > isize::MAX as usize
    {
        return None;
    }
    Some((start, start.checked_add(bytes)?))
}

fn regions_overlap(first: (usize, usize), second: (usize, usize)) -> bool {
    first.0 < second.1 && second.0 < first.1
}

/// Returns decoded frames, or -1 for an invalid format, length, address,
/// alignment, overlapping buffers, or capacity.
/// A failed call never writes to output.
///
/// # Safety
/// For nonempty regions, `input` must identify one live allocation containing
/// `input_length` initialized readable bytes, and `output` must identify one
/// live allocation with `input_length / (sample_width * channels)` initialized,
/// writable aligned `f32` values. Both allocations must remain valid for this call.
/// No other access may mutate input or access output during the call. The
/// function rejects numeric range, alignment and overlap errors; it cannot
/// validate allocation provenance, actual capacity, lifetime or concurrent use.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_pcm_decode_mono(
    input: *const u8,
    input_length: usize,
    bits: u32,
    is_float: u8,
    channels: usize,
    output: *mut f32,
    output_capacity: usize,
) -> isize {
    let Some(format) = PcmFormat::parse(bits, is_float) else {
        return -1;
    };
    let Some(frame_width) = frame_width(format, channels) else {
        return -1;
    };
    let frames = input_length / frame_width;
    let Some(input_region) = buffer_region(input, input_length) else {
        return -1;
    };
    let Some(output_region) = buffer_region(output.cast_const(), frames) else {
        return -1;
    };
    if output_capacity < frames || regions_overlap(input_region, output_region) {
        return -1;
    }
    let input = if input_length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(input, input_length) }
    };
    let output = if frames == 0 {
        &mut []
    } else {
        unsafe { std::slice::from_raw_parts_mut(output, frames) }
    };
    decode_first_channel(input, format, channels, output).map_or(-1, |count| count as isize)
}

/// Returns input frames consumed, or -1 for an invalid format, length, address,
/// alignment, or overlapping buffers.
/// A failed call never writes to output.
///
/// # Safety
/// For nonempty regions, `input` must identify one live allocation containing
/// `input_length` initialized readable aligned `f32` values, and `output` must
/// identify one live allocation containing `output_length` initialized writable
/// bytes (including any incomplete trailing frame). Both allocations must
/// remain valid for this call. No other access may mutate input or access
/// output during the call. The function rejects numeric range, alignment and
/// overlap errors; it cannot validate allocation provenance, actual capacity,
/// lifetime or concurrent use.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_pcm_encode_mono(
    input: *const f32,
    input_length: usize,
    bits: u32,
    is_float: u8,
    channels: usize,
    output: *mut u8,
    output_length: usize,
) -> isize {
    let Some(format) = PcmFormat::parse(bits, is_float) else {
        return -1;
    };
    if frame_width(format, channels).is_none() {
        return -1;
    }
    let Some(input_region) = buffer_region(input, input_length) else {
        return -1;
    };
    let Some(output_region) = buffer_region(output.cast_const(), output_length) else {
        return -1;
    };
    if regions_overlap(input_region, output_region) {
        return -1;
    }
    let input = if input_length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(input, input_length) }
    };
    let output = if output_length == 0 {
        &mut []
    } else {
        unsafe { std::slice::from_raw_parts_mut(output, output_length) }
    };
    encode_mono(input, format, channels, output).map_or(-1, |count| count as isize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_selects_first_interleaved_channel_and_preserves_partial_frame() {
        let bytes = [0x00, 0x80, 0xff, 0x7f, 0x00, 0x40, 0x00, 0x80, 0xaa];
        let mut output = [9.0; 3];
        assert_eq!(
            decode_first_channel(&bytes, PcmFormat::Signed16, 2, &mut output),
            Some(2)
        );
        assert_eq!(output, [-1.0, 0.5, 9.0]);
    }

    #[test]
    fn float_and_integer_widths_round_trip_and_fill_silence() {
        for format in [
            PcmFormat::Float32,
            PcmFormat::Float64,
            PcmFormat::Signed16,
            PcmFormat::Signed32,
        ] {
            let mut encoded = vec![0xa5; format.width() * 2 * 3 + 1];
            assert_eq!(encode_mono(&[-1.0, 2.0], format, 2, &mut encoded), Some(2));
            assert_eq!(*encoded.last().unwrap(), 0xa5);
            let mut decoded = [9.0; 3];
            assert_eq!(
                decode_first_channel(&encoded, format, 2, &mut decoded),
                Some(3)
            );
            assert_eq!(decoded[0], -1.0);
            assert!(decoded[1] >= 32767.0 / 32768.0 && decoded[1] <= 1.0);
            assert_eq!(decoded[2], 0.0);
            for frame in encoded[..encoded.len() - 1].chunks_exact(format.width() * 2) {
                assert_eq!(&frame[..format.width()], &frame[format.width()..]);
            }
        }
    }

    #[test]
    fn invalid_ffi_calls_do_not_mutate_output() {
        assert_eq!(switch_audio_pcm_format_supported(16, 0), 1);
        assert_eq!(switch_audio_pcm_format_supported(32, 0), 1);
        assert_eq!(switch_audio_pcm_format_supported(32, 1), 1);
        assert_eq!(switch_audio_pcm_format_supported(64, 1), 1);
        assert_eq!(switch_audio_pcm_format_supported(24, 0), 0);
        assert_eq!(switch_audio_pcm_format_supported(16, 1), 0);
        assert_eq!(switch_audio_pcm_format_supported(32, 2), 0);
        let input = [1.0_f32];
        let mut output = [0x5a_u8; 8];
        assert_eq!(
            unsafe {
                switch_audio_pcm_encode_mono(
                    input.as_ptr(),
                    1,
                    24,
                    0,
                    2,
                    output.as_mut_ptr(),
                    output.len(),
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                switch_audio_pcm_encode_mono(
                    std::ptr::null(),
                    1,
                    16,
                    0,
                    2,
                    output.as_mut_ptr(),
                    output.len(),
                )
            },
            -1
        );
        assert_eq!(output, [0x5a; 8]);
        let mut decoded = [9.0_f32];
        assert_eq!(
            unsafe {
                switch_audio_pcm_decode_mono(
                    output.as_ptr(),
                    output.len(),
                    16,
                    0,
                    2,
                    decoded.as_mut_ptr(),
                    1,
                )
            },
            -1
        );
        assert_eq!(decoded, [9.0]);
    }

    #[test]
    fn front_trim_keeps_interleaved_frames_aligned_at_real_queue_limit() {
        let maximum = 1_048_576;
        // Three signed-16 channels occupy six bytes per frame. The old
        // byte-count trim removed two bytes and changed the first channel.
        let queued = maximum + 2;
        assert_eq!(
            switch_audio_pcm_front_trim_bytes(queued, maximum, 16, 0, 3),
            6
        );
        assert_eq!((queued - 6) % 6, 0);
    }

    #[test]
    fn front_trim_preserves_split_callback_continuation() {
        let mut frames = Vec::new();
        for first_channel in [0_i16, 8192, 16384] {
            frames.extend_from_slice(&first_channel.to_le_bytes());
            frames.extend_from_slice(&100_i16.to_le_bytes());
            frames.extend_from_slice(&200_i16.to_le_bytes());
        }
        let mut queue = frames[..15].to_vec(); // two frames and half of the third
        let drop = switch_audio_pcm_front_trim_bytes(queue.len(), 12, 16, 0, 3);
        assert_eq!(drop, 6);
        queue.drain(..drop as usize);
        queue.extend_from_slice(&frames[15..]);
        assert_eq!(
            switch_audio_pcm_front_trim_bytes(queue.len(), 12, 16, 0, 3),
            0
        );
        let mut decoded = [0.0; 2];
        assert_eq!(
            decode_first_channel(&queue, PcmFormat::Signed16, 3, &mut decoded),
            Some(2)
        );
        assert_eq!(decoded, [0.25, 0.5]);
    }

    #[test]
    fn front_trim_keeps_supported_mono_stereo_and_first_noninterleaved_channel() {
        assert_eq!(switch_audio_pcm_front_trim_bytes(10, 8, 16, 0, 1), 2);
        assert_eq!(switch_audio_pcm_front_trim_bytes(24, 16, 32, 1, 2), 8);
        // A non-interleaved three-channel AudioBufferList queues only its
        // first buffer, so the adapter passes one channel to this helper.
        assert_eq!(switch_audio_pcm_front_trim_bytes(10, 8, 16, 0, 1), 2);
        assert_eq!(switch_audio_pcm_front_trim_bytes(8, 8, 16, 0, 1), 0);
    }

    #[test]
    fn front_trim_rejects_unsupported_or_impossible_formats() {
        // 24-bit integer PCM is not accepted by streamFormat or this core.
        assert_eq!(switch_audio_pcm_front_trim_bytes(12, 8, 24, 0, 1), -1);
        assert_eq!(switch_audio_pcm_front_trim_bytes(12, 8, 16, 0, 0), -1);
        assert_eq!(switch_audio_pcm_front_trim_bytes(12, 1, 16, 0, 1), -1);
        assert_eq!(
            switch_audio_pcm_front_trim_bytes(usize::MAX, 8, 16, 0, 1),
            -1
        );
    }

    #[test]
    fn ffi_converts_live_stereo_frames_and_silence() {
        let source = [f32::NAN, 0.5];
        let mut bytes = [0xff_u8; 12];
        assert_eq!(
            unsafe {
                switch_audio_pcm_encode_mono(
                    source.as_ptr(),
                    source.len(),
                    16,
                    0,
                    2,
                    bytes.as_mut_ptr(),
                    bytes.len(),
                )
            },
            2
        );
        assert_eq!(bytes, [0, 0, 0, 0, 0xff, 0x3f, 0xff, 0x3f, 0, 0, 0, 0]);
        let mut decoded = [9.0_f32; 3];
        assert_eq!(
            unsafe {
                switch_audio_pcm_decode_mono(
                    bytes.as_ptr(),
                    bytes.len(),
                    16,
                    0,
                    2,
                    decoded.as_mut_ptr(),
                    decoded.len(),
                )
            },
            3
        );
        assert_eq!(decoded, [0.0, 16383.0 / 32768.0, 0.0]);
    }

    #[test]
    fn ffi_rejects_oversized_slices_before_accessing_memory() {
        let input = [0.5_f32];
        let mut bytes = [0x5a_u8; 8];
        let mut decoded = [9.0_f32];
        for length in [isize::MAX as usize + 1, usize::MAX] {
            assert_eq!(
                unsafe {
                    switch_audio_pcm_decode_mono(
                        bytes.as_ptr(),
                        length,
                        16,
                        0,
                        1,
                        decoded.as_mut_ptr(),
                        usize::MAX,
                    )
                },
                -1
            );
            assert_eq!(
                unsafe {
                    switch_audio_pcm_encode_mono(
                        input.as_ptr(),
                        1,
                        16,
                        0,
                        1,
                        bytes.as_mut_ptr(),
                        length,
                    )
                },
                -1
            );
        }
        // Each input f32 requires four bytes, independently of output frames.
        for length in [isize::MAX as usize / 4 + 1, usize::MAX] {
            assert_eq!(
                unsafe {
                    switch_audio_pcm_encode_mono(
                        input.as_ptr(),
                        length,
                        16,
                        0,
                        1,
                        bytes.as_mut_ptr(),
                        bytes.len(),
                    )
                },
                -1
            );
        }
        // The input byte count fits isize, but decoded f32 bytes do not.
        assert_eq!(
            unsafe {
                switch_audio_pcm_decode_mono(
                    bytes.as_ptr(),
                    isize::MAX as usize,
                    16,
                    0,
                    1,
                    decoded.as_mut_ptr(),
                    usize::MAX,
                )
            },
            -1
        );
        assert_eq!(bytes, [0x5a; 8]);
        assert_eq!(decoded, [9.0]);
    }

    #[test]
    fn ffi_rejects_overlapping_and_misaligned_buffers_without_writes() {
        let mut shared = [0.5_f32; 4];
        let pointer = shared.as_mut_ptr();
        assert_eq!(
            unsafe { switch_audio_pcm_decode_mono(pointer.cast(), 8, 32, 1, 1, pointer.add(1), 2) },
            -1
        );
        assert_eq!(
            unsafe { switch_audio_pcm_encode_mono(pointer, 2, 32, 1, 1, pointer.add(1).cast(), 8) },
            -1
        );
        let mut bytes = [0x5a_u8; 8];
        let misaligned = unsafe { pointer.cast::<u8>().add(1).cast::<f32>() };
        assert_eq!(
            unsafe { switch_audio_pcm_decode_mono(bytes.as_ptr(), 4, 32, 1, 1, misaligned, 1) },
            -1
        );
        assert_eq!(
            unsafe { switch_audio_pcm_encode_mono(misaligned, 1, 32, 1, 1, bytes.as_mut_ptr(), 4) },
            -1
        );
        assert_eq!(shared, [0.5; 4]);
        assert_eq!(bytes, [0x5a; 8]);
    }

    #[test]
    fn ffi_accepts_empty_null_buffers_and_adjacent_disjoint_regions() {
        assert_eq!(
            unsafe {
                switch_audio_pcm_decode_mono(std::ptr::null(), 0, 16, 0, 1, std::ptr::null_mut(), 0)
            },
            0
        );
        assert_eq!(
            unsafe {
                switch_audio_pcm_encode_mono(std::ptr::null(), 0, 16, 0, 1, std::ptr::null_mut(), 0)
            },
            0
        );
        let mut samples = [0.5_f32, 9.0];
        let pointer = samples.as_mut_ptr();
        assert_eq!(
            unsafe { switch_audio_pcm_decode_mono(pointer.cast(), 4, 32, 1, 1, pointer.add(1), 1) },
            1
        );
        assert_eq!(samples, [0.5, 0.5]);
        assert_eq!(
            unsafe { switch_audio_pcm_encode_mono(pointer, 1, 32, 1, 1, pointer.add(1).cast(), 4) },
            1
        );
        assert_eq!(samples, [0.5, 0.5]);
    }

    #[test]
    fn buffer_region_rejects_address_wraparound() {
        let pointer = std::ptr::without_provenance::<u8>(usize::MAX);
        assert_eq!(buffer_region(pointer, 1), None);
    }
}
