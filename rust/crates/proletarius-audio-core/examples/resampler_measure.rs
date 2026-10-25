//! Synthetic signal and wall-clock measurements for the shipping Rust core.
//! They do not establish real device, operator, or subjective call quality.

use proletarius_audio_core::LinearResampler;
use std::{hint::black_box, time::Instant};

fn sine_gain_db(output_rate: f64, frequency: f64) -> f64 {
    let input: Vec<f32> = (0..12_000)
        .map(|sample| {
            (2.0 * std::f64::consts::PI * frequency * sample as f64 / 48_000.0).sin() as f32
        })
        .collect();
    let mut resampler = LinearResampler::new(48_000.0, output_rate).unwrap();
    let mut output = Vec::new();
    for frame in input.chunks(960) {
        output.extend(resampler.process(frame));
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

fn percentile(sorted: &[u128], fraction: f64) -> u128 {
    sorted[((sorted.len() - 1) as f64 * fraction).round() as usize]
}

fn main() {
    for (output_rate, pass_limit, stop_start) in
        [(16_000.0, 7_000.0, 9_000.0), (8_000.0, 3_400.0, 5_000.0)]
    {
        let filter = LinearResampler::new(48_000.0, output_rate).unwrap();
        let mut pass = Vec::new();
        let mut frequency = 500.0;
        while frequency <= pass_limit {
            pass.push((frequency, sine_gain_db(output_rate, frequency)));
            frequency += 500.0;
        }
        if pass_limit == 3_400.0 {
            pass.push((3_400.0, sine_gain_db(output_rate, 3_400.0)));
        }
        let mut stop = Vec::new();
        frequency = stop_start;
        while frequency <= 23_500.0 {
            stop.push((frequency, sine_gain_db(output_rate, frequency)));
            frequency += 500.0;
        }
        let worst_pass = pass
            .iter()
            .max_by(|left, right| left.1.abs().total_cmp(&right.1.abs()))
            .unwrap();
        let worst_stop = stop
            .iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .unwrap();
        println!(
            "48k->{output_rate:.0}: taps={} group_delay_ms={:.3}, worst_pass={:.3}dB@{:.0}Hz, worst_stop={:.3}dB@{:.0}Hz",
            filter.filter_taps(),
            (filter.filter_taps() - 1) as f64 / 2.0 / 48.0,
            worst_pass.1,
            worst_pass.0,
            worst_stop.1,
            worst_stop.0,
        );
        for target in if output_rate == 16_000.0 {
            [7_000.0, 9_000.0, 10_000.0]
        } else {
            [3_400.0, 5_000.0, 7_000.0]
        } {
            println!(
                "  {target:.0}Hz: {:.3}dB",
                sine_gain_db(output_rate, target)
            );
        }

        let frame: Vec<f32> = (0..960)
            .map(|sample| {
                (2.0 * std::f64::consts::PI * 1_000.0 * sample as f64 / 48_000.0).sin() as f32
            })
            .collect();
        let mut resampler = LinearResampler::new(48_000.0, output_rate).unwrap();
        for _ in 0..100 {
            black_box(resampler.process(&frame));
        }
        let mut durations = Vec::with_capacity(1_000);
        let mut output_samples = 0;
        for _ in 0..1_000 {
            let start = Instant::now();
            output_samples += black_box(resampler.process(black_box(&frame))).len();
            durations.push(start.elapsed().as_nanos());
        }
        durations.sort_unstable();
        let mean_ns = durations.iter().sum::<u128>() / durations.len() as u128;
        println!(
            "  20ms_frame_ns mean={} p50={} p95={} p99={} max={} output_samples={}",
            mean_ns,
            percentile(&durations, 0.50),
            percentile(&durations, 0.95),
            percentile(&durations, 0.99),
            durations[durations.len() - 1],
            output_samples,
        );
    }
}
