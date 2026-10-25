#![deny(unsafe_op_in_unsafe_fn)]

use std::{ffi::c_void, sync::Arc};

mod module_devices;
mod pcm_format;
mod reader_admission;

/// The app force-loads this archive, so this exported anchor also retains the
/// BLE protocol core inside the same archive and avoids duplicate Rust runtimes.
#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_ble_core_link_anchor() {
    proletarius_ble_core::switch_ble_core_link_anchor();
    proletarius_handoff_core::switch_handoff_link_anchor();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResamplerError {
    InvalidRate,
    InvalidBuffer,
}

/// Stateful single-channel resampler. Downsampling applies a causal,
/// linear-phase Kaiser-window FIR before the established linear interpolation.
/// The input history and phase cross callback boundaries without flushing.
#[derive(Debug, Clone)]
pub struct LinearResampler {
    in_rate_units: u64,
    out_rate_units: u64,
    position_units: u128,
    buffer: Vec<f32>,
    lowpass: Option<Arc<[f32]>>,
}

fn bessel_i0(value: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for order in 1..=32 {
        let ratio = value / (2.0 * f64::from(order));
        term *= ratio * ratio;
        sum += term;
        if term < sum * 1.0e-15 {
            break;
        }
    }
    sum
}

fn downsample_lowpass(in_rate: f64, out_rate: f64) -> Arc<[f32]> {
    // 0.2 * output rate is the designed transition width: for 48->8 kHz,
    // 3.4 kHz stays in-band and 5 kHz is rejected. The tap bound covers the
    // constructor's maximum 64:1 ratio while keeping work proportional to
    // output frames rather than input frames.
    let taps = ((24.0 * in_rate / out_rate).ceil() as usize).clamp(161, 1601) | 1;
    let center = (taps - 1) as f64 / 2.0;
    let cutoff = 0.5 * out_rate / in_rate;
    let beta = 7.5;
    let window_normalizer = bessel_i0(beta);
    let mut coefficients = Vec::with_capacity(taps);
    for index in 0..taps {
        let offset = index as f64 - center;
        let argument = 2.0 * std::f64::consts::PI * cutoff * offset;
        let sinc = if offset == 0.0 {
            2.0 * cutoff
        } else {
            2.0 * cutoff * argument.sin() / argument
        };
        let position = offset / center;
        let window = bessel_i0(beta * (1.0 - position * position).sqrt()) / window_normalizer;
        coefficients.push(sinc * window);
    }
    let gain: f64 = coefficients.iter().sum();
    coefficients
        .into_iter()
        .map(|coefficient| (coefficient / gain) as f32)
        .collect::<Arc<[f32]>>()
}

impl LinearResampler {
    pub fn new(in_rate: f64, out_rate: f64) -> Result<Self, ResamplerError> {
        const RATE_SCALE: f64 = 1_000_000.0;
        if !in_rate.is_finite()
            || !out_rate.is_finite()
            || in_rate <= 0.0
            || out_rate <= 0.0
            || in_rate > 768_000.0
            || out_rate > 768_000.0
            || !(1.0 / 64.0..=64.0).contains(&(in_rate / out_rate))
        {
            return Err(ResamplerError::InvalidRate);
        }
        let in_rate_units = (in_rate * RATE_SCALE).round() as u64;
        let out_rate_units = (out_rate * RATE_SCALE).round() as u64;
        if in_rate_units == 0 || out_rate_units == 0 {
            return Err(ResamplerError::InvalidRate);
        }
        Ok(Self {
            in_rate_units,
            out_rate_units,
            position_units: 0,
            buffer: Vec::new(),
            lowpass: (in_rate > out_rate).then(|| downsample_lowpass(in_rate, out_rate)),
        })
    }

    #[inline]
    fn sample_at(&self, index: usize) -> f64 {
        let Some(coefficients) = &self.lowpass else {
            return f64::from(self.buffer[index]);
        };
        let count = coefficients.len().min(index + 1);
        let mut value = 0.0_f64;
        for tap in 0..count {
            value += f64::from(coefficients[tap]) * f64::from(self.buffer[index - tap]);
        }
        value
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        self.buffer.extend_from_slice(input);
        let estimated = ((input.len() as f64 * self.out_rate_units as f64
            / self.in_rate_units as f64)
            .ceil() as usize)
            .saturating_add(2);
        let mut output = Vec::with_capacity(estimated);
        let denominator = u128::from(self.out_rate_units);
        while self.position_units < (self.buffer.len() - 1) as u128 * denominator {
            let index = (self.position_units / denominator) as usize;
            let fraction = (self.position_units % denominator) as f64 / denominator as f64;
            let first = self.sample_at(index);
            let second = self.sample_at(index + 1);
            output.push((first + (second - first) * fraction) as f32);
            self.position_units += u128::from(self.in_rate_units);
        }

        // Preserve the FIR's past input as well as the interpolation endpoint.
        // Subtract only samples actually removed so arbitrary chunking has the
        // same phase and convolution history as a single large input block.
        let consumed = (self.position_units / denominator).min(usize::MAX as u128) as usize;
        let removable = self.buffer.len().saturating_sub(1);
        let history = self.lowpass.as_ref().map_or(1, |filter| filter.len());
        let drop_count = consumed.saturating_sub(history).min(removable);
        if drop_count > 0 {
            self.buffer.drain(..drop_count);
            self.position_units -= drop_count as u128 * denominator;
        }
        output
    }

    pub fn reset(&mut self) {
        self.position_units = 0;
        self.buffer.clear();
    }

    pub fn filter_taps(&self) -> usize {
        self.lowpass.as_ref().map_or(0, |filter| filter.len())
    }
}

pub fn f32_to_i16(sample: f32) -> i16 {
    if sample.is_nan() {
        return 0;
    }
    if sample <= -1.0 {
        return i16::MIN;
    }
    if sample >= 1.0 {
        return i16::MAX;
    }
    let scale = if sample < 0.0 { 32768.0 } else { 32767.0 };
    (f64::from(sample) * scale).trunc() as i16
}

pub fn f32_to_i32(sample: f32) -> i32 {
    if sample.is_nan() {
        return 0;
    }
    if sample <= -1.0 {
        return i32::MIN;
    }
    if sample >= 1.0 {
        return i32::MAX;
    }
    let scale = if sample < 0.0 {
        2_147_483_648.0
    } else {
        2_147_483_647.0
    };
    (f64::from(sample) * scale).trunc() as i32
}

pub fn sanitize_f32(sample: f32) -> f32 {
    if sample.is_nan() {
        0.0
    } else {
        sample.clamp(-1.0, 1.0)
    }
}

/// Core route guard. CoreAudio discovery stays in Swift; route direction and
/// device identity policy is evaluated here from stable IDs and UID matches.
pub fn route_is_safe(
    default_output: u32,
    default_input: u32,
    module_input: u32,
    module_output: u32,
    output_shares_module_uid: bool,
    input_shares_module_uid: bool,
) -> bool {
    default_output != 0
        && default_input != 0
        && module_input != 0
        && module_output != 0
        && default_output != module_output
        && default_input != module_input
        && !output_shares_module_uid
        && !input_shares_module_uid
}

pub fn recovery_is_current(
    captured_session: u64,
    current_session: u64,
    captured_recovery: u64,
    current_recovery: u64,
    running: bool,
) -> bool {
    running && captured_session == current_session && captured_recovery == current_recovery
}

/// Playback decisions for 20 ms reader-call PCM buffers. The app keeps the
/// AVAudioPCMBuffer objects; this state machine owns only counts and epochs.
/// No transition allocates or waits on a backend process.
#[derive(Debug, Clone, Copy)]
pub struct PlaybackQueuePolicy {
    generation: u64,
    pending: u8,
    queued: u8,
    primed: bool,
    prime_frames: u8,
    underruns: u64,
}

impl Default for PlaybackQueuePolicy {
    fn default() -> Self {
        Self {
            generation: 0,
            pending: 0,
            queued: 0,
            primed: false,
            prime_frames: 6, // 120 ms at 20 ms per frame
            underruns: 0,
        }
    }
}

impl PlaybackQueuePolicy {
    /// 0 = hold, 1 = schedule this buffer, 2 = schedule pending and play,
    /// 3 = drop because the scheduled queue is full.
    pub fn receive(&mut self) -> u8 {
        if !self.primed {
            self.pending += 1;
            if self.pending < self.prime_frames {
                return 0;
            }
            self.primed = true;
            self.queued = self.pending;
            self.pending = 0;
            return 2;
        }
        if self.queued >= 20 {
            return 3;
        }
        self.queued += 1;
        1
    }

    /// 0 = ignore stale completion, 1 = count completion, 2 = pause/re-prime.
    pub fn played(&mut self, captured_generation: u64, running: bool) -> u8 {
        if !running || captured_generation != self.generation || self.queued == 0 {
            return 0;
        }
        self.queued -= 1;
        if self.queued != 0 {
            return 1;
        }
        self.primed = false;
        self.underruns += 1;
        self.prime_frames = (self.prime_frames + 2).min(12);
        2
    }

    pub fn reset_for_route(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = 0;
        self.queued = 0;
        self.primed = false;
    }

    pub fn reset_for_session(&mut self) {
        self.reset_for_route();
        self.prime_frames = 6;
        self.underruns = 0;
    }
}

/// Stateful reader-VoWiFi microphone processing kept identical to the former
/// Swift bridge: 100 Hz high-pass, adaptive noise floor/gate, smoothed gain,
/// and a 0.90 peak limiter. CoreAudio only supplies and schedules PCM frames.
#[derive(Debug, Clone)]
pub struct MicrophoneConditioner {
    high_pass_coefficient: f32,
    previous_input: f32,
    previous_output: f32,
    noise_floor: f32,
    gate_gain: f32,
    gate_open: bool,
    gate_hold_frames: u8,
    filtered: Vec<f32>,
}

impl MicrophoneConditioner {
    pub fn new(sample_rate: u32) -> Option<Self> {
        if !(8_000..=48_000).contains(&sample_rate) {
            return None;
        }
        Some(Self {
            high_pass_coefficient: (-2.0_f32 * std::f32::consts::PI * 100.0 / sample_rate as f32)
                .exp(),
            previous_input: 0.0,
            previous_output: 0.0,
            noise_floor: 0.003,
            gate_gain: 1.0,
            gate_open: true,
            gate_hold_frames: 0,
            filtered: Vec::with_capacity((sample_rate / 50) as usize),
        })
    }

    pub fn reset(&mut self) {
        self.previous_input = 0.0;
        self.previous_output = 0.0;
        self.noise_floor = 0.003;
        self.gate_gain = 1.0;
        self.gate_open = true;
        self.gate_hold_frames = 0;
        self.filtered.clear();
    }

    /// Mirrors the legacy route-change reset, which preserved its learned
    /// noise floor while clearing filter history and gate state.
    pub fn reset_stream(&mut self) {
        self.previous_input = 0.0;
        self.previous_output = 0.0;
        self.gate_gain = 1.0;
        self.gate_open = true;
        self.gate_hold_frames = 0;
        self.filtered.clear();
    }

    pub fn process_frame(&mut self, frame: &mut [i16], system_voice_processing_active: bool) {
        if frame.is_empty() {
            return;
        }
        self.filtered.clear();
        self.filtered.reserve(frame.len());
        let mut energy = 0.0_f32;
        let mut peak = 0.0_f32;
        for sample in frame.iter().copied() {
            let input = f32::from(sample) / 32_768.0;
            let output =
                self.high_pass_coefficient * (self.previous_output + input - self.previous_input);
            self.previous_input = input;
            self.previous_output = output;
            self.filtered.push(output);
            energy += output * output;
            peak = peak.max(output.abs());
        }

        let rms = (energy / frame.len() as f32).sqrt();
        if rms < 0.02_f32.max(self.noise_floor * 3.0) {
            self.noise_floor = self.noise_floor * 0.98 + rms * 0.02;
        }
        let threshold = 0.0035_f32.max(self.noise_floor * 2.4);
        if rms >= threshold * 1.15 {
            self.gate_open = true;
            self.gate_hold_frames = 8;
        } else if self.gate_hold_frames > 0 {
            self.gate_hold_frames -= 1;
        } else if rms < threshold * 0.8 {
            self.gate_open = false;
        }

        let target_gain =
            if system_voice_processing_active || self.gate_open || self.gate_hold_frames > 0 {
                1.0
            } else {
                0.30_f32.max(rms / threshold)
            };
        let smoothing = if target_gain > self.gate_gain {
            0.45
        } else {
            0.06
        };
        self.gate_gain += (target_gain - self.gate_gain) * smoothing;

        let limiter_gain = if peak > 0.90 { 0.90 / peak } else { 1.0 };
        let gain = self.gate_gain * limiter_gain;
        for (sample, value) in frame.iter_mut().zip(self.filtered.iter().copied()) {
            let value = (value * gain).clamp(-0.95, 0.95);
            *sample = (value * 32_767.0) as i16;
        }
    }
}

#[repr(C)]
struct ResamplerHandle {
    resampler: LinearResampler,
}

#[repr(C)]
struct MicrophoneConditionerHandle {
    conditioner: MicrophoneConditioner,
}

#[repr(C)]
struct PlaybackQueuePolicyHandle {
    policy: PlaybackQueuePolicy,
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_playback_policy_create() -> *mut c_void {
    Box::into_raw(Box::new(PlaybackQueuePolicyHandle {
        policy: PlaybackQueuePolicy::default(),
    }))
    .cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_playback_policy_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<PlaybackQueuePolicyHandle>()) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_playback_policy_receive(handle: *mut c_void) -> u8 {
    unsafe { handle.cast::<PlaybackQueuePolicyHandle>().as_mut() }
        .map_or(3, |handle| handle.policy.receive())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_playback_policy_played(
    handle: *mut c_void,
    captured_generation: u64,
    running: u8,
) -> u8 {
    unsafe { handle.cast::<PlaybackQueuePolicyHandle>().as_mut() }.map_or(0, |handle| {
        handle.policy.played(captured_generation, running != 0)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_playback_policy_reset(handle: *mut c_void, session: u8) {
    if let Some(handle) = unsafe { handle.cast::<PlaybackQueuePolicyHandle>().as_mut() } {
        if session != 0 {
            handle.policy.reset_for_session();
        } else {
            handle.policy.reset_for_route();
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_playback_policy_generation(handle: *const c_void) -> u64 {
    unsafe { handle.cast::<PlaybackQueuePolicyHandle>().as_ref() }
        .map_or(0, |handle| handle.policy.generation)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_playback_policy_queued(handle: *const c_void) -> u8 {
    unsafe { handle.cast::<PlaybackQueuePolicyHandle>().as_ref() }
        .map_or(0, |handle| handle.policy.queued)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_playback_policy_underruns(handle: *const c_void) -> u64 {
    unsafe { handle.cast::<PlaybackQueuePolicyHandle>().as_ref() }
        .map_or(0, |handle| handle.policy.underruns)
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_microphone_conditioner_create(sample_rate: u32) -> *mut c_void {
    let Some(conditioner) = MicrophoneConditioner::new(sample_rate) else {
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(MicrophoneConditionerHandle { conditioner })).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_microphone_conditioner_reset(handle: *mut c_void) {
    if let Some(handle) = unsafe { handle.cast::<MicrophoneConditionerHandle>().as_mut() } {
        handle.conditioner.reset();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_microphone_conditioner_reset_stream(handle: *mut c_void) {
    if let Some(handle) = unsafe { handle.cast::<MicrophoneConditionerHandle>().as_mut() } {
        handle.conditioner.reset_stream();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_microphone_conditioner_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<MicrophoneConditionerHandle>()) });
    }
}

/// Processes one mutable signed-16 PCM frame; returns 1 on success, 0 on bad input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_microphone_conditioner_process(
    handle: *mut c_void,
    frame: *mut i16,
    frame_len: usize,
    system_voice_processing_active: u8,
) -> u8 {
    let Some(handle) = (unsafe { handle.cast::<MicrophoneConditionerHandle>().as_mut() }) else {
        return 0;
    };
    if frame_len == 0 {
        return 1;
    }
    if frame.is_null() {
        return 0;
    }
    let frame = unsafe { std::slice::from_raw_parts_mut(frame, frame_len) };
    handle
        .conditioner
        .process_frame(frame, system_voice_processing_active != 0);
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_f32_to_i16(sample: f32) -> i16 {
    f32_to_i16(sample)
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_f32_to_i32(sample: f32) -> i32 {
    f32_to_i32(sample)
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_sanitize_f32(sample: f32) -> f32 {
    sanitize_f32(sample)
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_route_is_safe(
    default_output: u32,
    default_input: u32,
    module_input: u32,
    module_output: u32,
    output_shares_module_uid: u8,
    input_shares_module_uid: u8,
) -> u8 {
    u8::from(route_is_safe(
        default_output,
        default_input,
        module_input,
        module_output,
        output_shares_module_uid != 0,
        input_shares_module_uid != 0,
    ))
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_recovery_is_current(
    captured_session: u64,
    current_session: u64,
    captured_recovery: u64,
    current_recovery: u64,
    running: u8,
) -> u8 {
    u8::from(recovery_is_current(
        captured_session,
        current_session,
        captured_recovery,
        current_recovery,
        running != 0,
    ))
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_resampler_create(in_rate: f64, out_rate: f64) -> *mut c_void {
    let Ok(resampler) = LinearResampler::new(in_rate, out_rate) else {
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(ResamplerHandle { resampler })).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_resampler_reset(handle: *mut c_void) {
    if let Some(handle) = unsafe { handle.cast::<ResamplerHandle>().as_mut() } {
        handle.resampler.reset();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_resampler_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<ResamplerHandle>()) });
    }
}

/// Returns the number of output samples or -1 when pointers/capacity are bad.
/// If capacity is insufficient the input is not consumed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_resampler_process(
    handle: *mut c_void,
    input: *const f32,
    input_len: usize,
    output: *mut f32,
    output_capacity: usize,
) -> isize {
    if handle.is_null() || (input_len > 0 && input.is_null()) || output.is_null() {
        return -1;
    }
    let Some(handle) = (unsafe { handle.cast::<ResamplerHandle>().as_mut() }) else {
        return -1;
    };
    if input_len == 0 {
        return 0;
    }
    let input = unsafe { std::slice::from_raw_parts(input, input_len) };
    let output = unsafe { std::slice::from_raw_parts_mut(output, output_capacity) };
    // Clone current state so an undersized caller buffer cannot partially
    // consume a realtime chunk. Swift allocates using the same conservative
    // input-rate ratio plus the held interpolation sample.
    let mut candidate = handle.resampler.clone();
    let samples = candidate.process(input);
    if samples.len() > output.len() || samples.len() > isize::MAX as usize {
        return -1;
    }
    output[..samples.len()].copy_from_slice(&samples);
    handle.resampler = candidate;
    samples.len() as isize
}

#[cfg(test)]
mod tests {
    use super::{
        LinearResampler, MicrophoneConditioner, PlaybackQueuePolicy, f32_to_i16, f32_to_i32,
        recovery_is_current, route_is_safe, sanitize_f32,
    };

    #[test]
    fn playback_policy_primes_caps_underruns_and_retires_late_completions() {
        let mut policy = PlaybackQueuePolicy::default();
        for _ in 0..5 {
            assert_eq!(policy.receive(), 0);
        }
        assert_eq!(policy.receive(), 2);
        assert_eq!(policy.queued, 6);
        for _ in 0..14 {
            assert_eq!(policy.receive(), 1);
        }
        assert_eq!(policy.queued, 20);
        assert_eq!(policy.receive(), 3);
        for _ in 0..19 {
            assert_eq!(policy.played(0, true), 1);
        }
        assert_eq!(policy.played(0, true), 2);
        assert_eq!(
            (policy.prime_frames, policy.underruns, policy.queued),
            (8, 1, 0)
        );
        assert_eq!(policy.played(0, true), 0);

        for expected_prime in [8, 10, 12, 12] {
            for _ in 1..expected_prime {
                assert_eq!(policy.receive(), 0);
            }
            assert_eq!(policy.receive(), 2);
            assert_eq!(policy.queued, expected_prime);
            for _ in 1..expected_prime {
                assert_eq!(policy.played(0, true), 1);
            }
            assert_eq!(policy.played(0, true), 2);
        }
        assert_eq!((policy.prime_frames, policy.underruns), (12, 5));

        assert_eq!(policy.receive(), 0);
        policy.reset_for_route();
        assert_eq!(
            (policy.pending, policy.queued, policy.prime_frames),
            (0, 0, 12)
        );
        assert_eq!(policy.played(0, true), 0);
        for _ in 0..11 {
            assert_eq!(policy.receive(), 0);
        }
        assert_eq!(policy.receive(), 2);
        assert_eq!(policy.played(1, false), 0);
        policy.reset_for_session();
        assert_eq!(policy.played(1, true), 0);
        assert_eq!(
            (policy.generation, policy.prime_frames, policy.underruns),
            (2, 6, 0)
        );
    }

    #[test]
    fn playback_policy_ffi_actions_and_null_handles_are_safe() {
        use super::{
            switch_audio_playback_policy_create, switch_audio_playback_policy_destroy,
            switch_audio_playback_policy_generation, switch_audio_playback_policy_played,
            switch_audio_playback_policy_queued, switch_audio_playback_policy_receive,
            switch_audio_playback_policy_reset, switch_audio_playback_policy_underruns,
        };
        let handle = switch_audio_playback_policy_create();
        assert!(!handle.is_null());
        for _ in 0..5 {
            assert_eq!(unsafe { switch_audio_playback_policy_receive(handle) }, 0);
        }
        assert_eq!(unsafe { switch_audio_playback_policy_receive(handle) }, 2);
        assert_eq!(unsafe { switch_audio_playback_policy_queued(handle) }, 6);
        unsafe { switch_audio_playback_policy_reset(handle, 0) };
        assert_eq!(
            unsafe { switch_audio_playback_policy_generation(handle) },
            1
        );
        assert_eq!(
            unsafe { switch_audio_playback_policy_played(handle, 0, 1) },
            0
        );
        assert_eq!(unsafe { switch_audio_playback_policy_underruns(handle) }, 0);
        unsafe { switch_audio_playback_policy_destroy(handle) };
        assert_eq!(
            unsafe { switch_audio_playback_policy_receive(std::ptr::null_mut()) },
            3
        );
        assert_eq!(
            unsafe { switch_audio_playback_policy_played(std::ptr::null_mut(), 0, 1) },
            0
        );
        unsafe { switch_audio_playback_policy_destroy(std::ptr::null_mut()) };
    }

    fn conditioner_input(kind: usize, frame: usize) -> Vec<i16> {
        (0..320)
            .map(|index| {
                let sample = (frame * 320 + index) as i32;
                match kind {
                    0 => 0,
                    1 => (sample * 37 % 81 - 40) as i16,
                    2 => (sample * 137 % 2_401 - 1_200) as i16,
                    3 => {
                        if index % 5 == 0 {
                            i16::MAX
                        } else {
                            i16::MIN
                        }
                    }
                    4 => (sample * 251 % 18_001 - 9_000) as i16,
                    _ => 0,
                }
            })
            .collect()
    }

    fn frame_hash(frame: &[i16]) -> u64 {
        frame
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .fold(0xcbf29ce484222325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            })
    }

    #[test]
    fn microphone_conditioner_matches_legacy_swift_frame_vectors() {
        let sequence: &[(usize, bool, u64)] = &[
            (0, false, 0x96554ce2cfb01525),
            (1, false, 0xd9b89818973d788b),
            (1, false, 0xd405e11c614271b1),
            (2, false, 0x2b089a0bf1770ad6),
            (3, false, 0xbd7c7d25c87b93e5),
            (4, false, 0x18157009252da032),
            (0, false, 0xb11d69f072a59f5d),
            (0, false, 0x96554ce2cfb01525),
            (0, false, 0x96554ce2cfb01525),
            (0, false, 0x96554ce2cfb01525),
            (0, false, 0x96554ce2cfb01525),
            (0, false, 0x96554ce2cfb01525),
            (0, false, 0x96554ce2cfb01525),
            (0, false, 0x96554ce2cfb01525),
            (0, false, 0x96554ce2cfb01525),
            (1, true, 0xce96fc6f9616a328),
            (0, true, 0x96554ce2cfb01525),
            (2, false, 0x2b18fbb6f7a75159),
        ];
        let mut conditioner = MicrophoneConditioner::new(16_000).unwrap();
        for (index, (kind, system_processing, expected)) in sequence.iter().copied().enumerate() {
            let mut frame = conditioner_input(kind, index);
            conditioner.process_frame(&mut frame, system_processing);
            assert_eq!(frame_hash(&frame), expected, "legacy frame vector {index}");
        }

        conditioner.reset();
        let mut reset_frame = conditioner_input(2, 0);
        conditioner.process_frame(&mut reset_frame, false);
        assert_eq!(frame_hash(&reset_frame), 0x3fe16897fdb01945);
        assert!(MicrophoneConditioner::new(7_999).is_none());
        assert!(MicrophoneConditioner::new(48_001).is_none());
    }

    #[test]
    fn microphone_route_reset_preserves_adaptive_noise_floor() {
        let mut conditioner = MicrophoneConditioner::new(16_000).unwrap();
        let mut noise = conditioner_input(1, 0);
        conditioner.process_frame(&mut noise, false);
        let learned_floor = conditioner.noise_floor;
        conditioner.reset_stream();
        assert_eq!(conditioner.noise_floor, learned_floor);
        assert_eq!(conditioner.previous_input, 0.0);
        assert_eq!(conditioner.previous_output, 0.0);
        assert_eq!(conditioner.gate_gain, 1.0);
        assert!(conditioner.gate_open);
        assert_eq!(conditioner.gate_hold_frames, 0);
    }

    #[test]
    fn microphone_conditioner_ffi_validates_and_processes_frames() {
        use super::{
            switch_audio_microphone_conditioner_create,
            switch_audio_microphone_conditioner_destroy,
            switch_audio_microphone_conditioner_process, switch_audio_microphone_conditioner_reset,
            switch_audio_microphone_conditioner_reset_stream,
        };

        assert!(switch_audio_microphone_conditioner_create(7_999).is_null());
        let handle = switch_audio_microphone_conditioner_create(16_000);
        assert!(!handle.is_null());
        let mut samples = conditioner_input(2, 0);
        let status = unsafe {
            switch_audio_microphone_conditioner_process(
                handle,
                samples.as_mut_ptr(),
                samples.len(),
                0,
            )
        };
        assert_eq!(status, 1);
        assert_eq!(
            unsafe {
                switch_audio_microphone_conditioner_process(handle, std::ptr::null_mut(), 1, 0)
            },
            0
        );
        assert_eq!(
            unsafe {
                switch_audio_microphone_conditioner_process(handle, std::ptr::null_mut(), 0, 0)
            },
            1
        );
        assert_eq!(
            unsafe {
                switch_audio_microphone_conditioner_process(
                    std::ptr::null_mut(),
                    samples.as_mut_ptr(),
                    samples.len(),
                    0,
                )
            },
            0
        );
        unsafe {
            switch_audio_microphone_conditioner_reset_stream(handle);
            switch_audio_microphone_conditioner_reset(handle);
            switch_audio_microphone_conditioner_destroy(handle);
            switch_audio_microphone_conditioner_destroy(std::ptr::null_mut());
        }
    }

    #[test]
    fn signed_pcm_conversions_saturate_and_define_non_finite_inputs() {
        assert_eq!(f32_to_i16(-1.0), i16::MIN);
        assert_eq!(f32_to_i16(1.0), i16::MAX);
        assert_eq!(f32_to_i16(f32::NAN), 0);
        assert_eq!(f32_to_i16(f32::NEG_INFINITY), i16::MIN);
        assert_eq!(f32_to_i16(f32::INFINITY), i16::MAX);
        assert_eq!(f32_to_i32(-1.0), i32::MIN);
        assert_eq!(f32_to_i32(1.0), i32::MAX);
        assert_eq!(f32_to_i32(f32::NAN), 0);
        assert_eq!(f32_to_i32(f32::NEG_INFINITY), i32::MIN);
        assert_eq!(f32_to_i32(f32::INFINITY), i32::MAX);
        assert_eq!(sanitize_f32(f32::NAN), 0.0);
        assert_eq!(sanitize_f32(f32::NEG_INFINITY), -1.0);
        assert_eq!(sanitize_f32(f32::INFINITY), 1.0);
        assert_eq!(
            f32_to_i32(f32::from_bits(1.0f32.to_bits() - 1)),
            i32::MAX - 128
        );
        assert_eq!(
            f32_to_i32(f32::from_bits((-1.0f32).to_bits() - 1)),
            i32::MIN + 128
        );
    }

    #[test]
    fn downsample_output_does_not_depend_on_input_chunk_boundaries() {
        let source: Vec<f32> = (0..1024).map(|sample| sample as f32).collect();
        let mut whole = LinearResampler::new(48_000.0, 8_000.0).unwrap();
        let whole_output = whole.process(&source);

        let mut chunked = LinearResampler::new(48_000.0, 8_000.0).unwrap();
        let mut chunked_output = chunked.process(&source[..512]);
        chunked_output.extend(chunked.process(&source[512..]));
        assert_eq!(whole_output, chunked_output);
        assert_eq!(whole_output.len(), 171);
        // The causal 161-tap FIR adds 80 source samples of group delay.
        assert!((whole_output[86] - 436.0).abs() < 0.01);
    }

    #[test]
    fn streaming_resampler_keeps_long_run_rational_sample_count_and_phase() {
        // Exercise more than 1,000 CoreAudio-sized callbacks without retaining the
        // output. Exact integer-rate phase must not accumulate a fractional
        // sample of drift across callback boundaries.
        const INPUT_RATE: u64 = 48_000;
        const OUTPUT_RATE: u64 = 8_000;
        const TOTAL_INPUT: usize = 48_000 * 30;
        let mut resampler = LinearResampler::new(INPUT_RATE as f64, OUTPUT_RATE as f64).unwrap();
        let mut produced = 0usize;
        let mut supplied = 0usize;
        let mut callback = 0usize;
        while supplied < TOTAL_INPUT {
            let requested = 257 + (callback * 521 % 1_151);
            let count = requested.min(TOTAL_INPUT - supplied);
            produced += resampler.process(&vec![0.0; count]).len();
            supplied += count;
            callback += 1;
        }
        let numerator = (TOTAL_INPUT as u128 - 1) * u128::from(OUTPUT_RATE);
        let expected = numerator.div_ceil(u128::from(INPUT_RATE)) as usize;
        assert_eq!(produced, expected);
        assert_eq!(
            resampler.position_units % u128::from(resampler.out_rate_units),
            (produced as u128 * u128::from(resampler.in_rate_units))
                % u128::from(resampler.out_rate_units)
        );
    }

    #[test]
    fn streaming_resampler_retains_a_bounded_history_at_worst_rate_ratio() {
        let mut resampler = LinearResampler::new(64_000.0, 1_000.0).unwrap();
        let taps = resampler.filter_taps();
        assert_eq!(taps, 1_537);
        for callback in 0..512usize {
            let count = 1 + (callback * 193 % 4_096);
            let _ = resampler.process(&vec![0.0; count]);
            assert!(
                resampler.buffer.len() <= taps + 65,
                "retained {} samples after callback {callback}",
                resampler.buffer.len()
            );
        }
    }

    #[test]
    fn downsample_fir_has_the_declared_linear_phase_group_delay() {
        for (input_rate, output_rate) in [
            (48_000.0, 16_000.0),
            (48_000.0, 8_000.0),
            (64_000.0, 1_000.0),
        ] {
            let resampler = LinearResampler::new(input_rate, output_rate).unwrap();
            let coefficients = resampler.lowpass.as_ref().expect("downsample FIR");
            let delay = (coefficients.len() - 1) / 2;
            assert_eq!(delay * 2 + 1, coefficients.len());
            for index in 0..=delay {
                assert!(
                    (coefficients[index] - coefficients[coefficients.len() - 1 - index]).abs()
                        <= 2.0 * f32::EPSILON,
                    "asymmetric tap {index} for {input_rate}->{output_rate}"
                );
            }
            let gain: f64 = coefficients.iter().map(|value| f64::from(*value)).sum();
            assert!((gain - 1.0).abs() <= 1.0e-6);
            assert_eq!(
                delay,
                (resampler.filter_taps() - 1) / 2,
                "group delay must be expressed in source samples"
            );
        }
    }

    fn sine_gain_db(input_rate: f64, output_rate: f64, frequency: f64) -> f64 {
        let input: Vec<f32> = (0..48_000)
            .map(|sample| {
                (2.0 * std::f64::consts::PI * frequency * sample as f64 / input_rate).sin() as f32
            })
            .collect();
        let mut resampler = LinearResampler::new(input_rate, output_rate).unwrap();
        let mut output = Vec::new();
        for chunk in input.chunks(960) {
            output.extend(resampler.process(chunk));
        }
        let settled = &output[(output.len() / 10)..];
        let rms = (settled
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>()
            / settled.len() as f64)
            .sqrt();
        20.0 * (rms / std::f64::consts::FRAC_1_SQRT_2).log10()
    }

    #[test]
    fn downsampling_rejects_specified_alias_bands_without_losing_speech_passband() {
        for (output_rate, passband, stopband) in [
            (
                16_000.0,
                &[1_000.0, 3_000.0, 5_000.0, 7_000.0][..],
                &[9_000.0, 10_000.0, 12_000.0, 15_000.0, 20_000.0][..],
            ),
            (
                8_000.0,
                &[500.0, 1_000.0, 2_000.0, 3_400.0][..],
                &[5_000.0, 6_000.0, 7_000.0, 9_000.0, 12_000.0][..],
            ),
        ] {
            for &frequency in passband {
                let gain = sine_gain_db(48_000.0, output_rate, frequency);
                assert!(
                    gain.abs() <= 0.5,
                    "48k->{output_rate} {frequency} Hz: {gain:.2} dB"
                );
            }
            for &frequency in stopband {
                let gain = sine_gain_db(48_000.0, output_rate, frequency);
                assert!(
                    gain <= -60.0,
                    "48k->{output_rate} {frequency} Hz: {gain:.2} dB"
                );
            }
        }
    }

    #[test]
    fn randomized_chunking_preserves_exact_linear_sample_sequence() {
        let source: Vec<f32> = (0..48_000).map(|sample| (sample % 997) as f32).collect();
        for (in_rate, out_rate) in [
            (48_000.0, 8_000.0),
            (48_000.0, 44_100.0),
            (8_000.0, 48_000.0),
        ] {
            let mut whole = LinearResampler::new(in_rate, out_rate).unwrap();
            let expected = whole.process(&source);
            let mut chunked = LinearResampler::new(in_rate, out_rate).unwrap();
            let mut actual = Vec::new();
            let mut start = 0;
            let mut step = 1usize;
            while start < source.len() {
                let size = (step * 37 % 1_021 + 1).min(source.len() - start);
                actual.extend(chunked.process(&source[start..start + size]));
                start += size;
                step += 1;
            }
            assert_eq!(actual.len(), expected.len(), "rates {in_rate}->{out_rate}");
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                assert!(
                    (actual - expected).abs() <= 1.0e-6,
                    "rates {in_rate}->{out_rate}, index {index}: {actual} != {expected}"
                );
            }
        }
    }

    #[test]
    fn route_guard_rejects_wrong_direction_same_device_and_zero_ids() {
        assert!(route_is_safe(1, 2, 3, 4, false, false));
        assert!(!route_is_safe(4, 2, 3, 4, false, false));
        assert!(!route_is_safe(1, 3, 3, 4, false, false));
        assert!(!route_is_safe(1, 2, 3, 4, true, false));
        assert!(!route_is_safe(1, 2, 3, 4, false, true));
        assert!(!route_is_safe(0, 2, 3, 4, false, false));
    }

    #[test]
    fn delayed_recovery_requires_the_same_live_session_and_ticket() {
        assert!(recovery_is_current(8, 8, 3, 3, true));
        assert!(!recovery_is_current(8, 9, 3, 3, true));
        assert!(!recovery_is_current(8, 8, 3, 4, true));
        assert!(!recovery_is_current(8, 8, 3, 3, false));
    }

    #[test]
    fn ffi_resampler_does_not_consume_input_when_output_capacity_is_short() {
        let handle = super::switch_audio_resampler_create(48_000.0, 8_000.0);
        assert!(!handle.is_null());
        let input: Vec<f32> = (0..512).map(|sample| sample as f32).collect();
        let mut short = [0.0f32; 1];
        let short_count = unsafe {
            super::switch_audio_resampler_process(
                handle,
                input.as_ptr(),
                input.len(),
                short.as_mut_ptr(),
                short.len(),
            )
        };
        assert_eq!(short_count, -1);

        let mut output = [0.0f32; 100];
        let count = unsafe {
            super::switch_audio_resampler_process(
                handle,
                input.as_ptr(),
                input.len(),
                output.as_mut_ptr(),
                output.len(),
            )
        };
        assert_eq!(count, 86);
        let expected = LinearResampler::new(48_000.0, 8_000.0)
            .unwrap()
            .process(&input);
        assert_eq!(
            &output[..count as usize],
            expected.as_slice(),
            "short output must not advance FIR history or resampler phase"
        );
        unsafe { super::switch_audio_resampler_destroy(handle) };
    }
}
