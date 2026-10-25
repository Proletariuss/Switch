//! Historical euicc-go BPP segmentation oracles; only in-memory card traffic.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use proletarius_euicc::{CardTransport, Euicc};

struct SegmentRecorder {
    expected_count: usize,
    segments: Vec<Vec<u8>>,
    pending: Vec<u8>,
    chunk_index: usize,
}

impl CardTransport for SegmentRecorder {
    type Error = &'static str;

    fn transmit(&mut self, command: &[u8], response: &mut Vec<u8>) -> Result<(), Self::Error> {
        assert!(command.len() >= 5);
        assert_eq!(&command[..2], &[0x80, 0xe2]);
        assert!(matches!(command[2], 0x11 | 0x91));
        assert_eq!(usize::from(command[3]), self.chunk_index);
        assert_eq!(usize::from(command[4]), command.len() - 5);
        self.pending.extend_from_slice(&command[5..]);
        self.chunk_index += 1;
        response.clear();
        if command[2] == 0x91 {
            self.segments.push(std::mem::take(&mut self.pending));
            self.chunk_index = 0;
            assert!(self.segments.len() <= self.expected_count);
            if self.segments.len() == self.expected_count {
                // Synthetic success: transaction aa, installed ISD-P AID bb.
                response.extend_from_slice(&[
                    0xbf, 0x37, 0x0d, 0xbf, 0x27, 0x0a, 0x80, 0x01, 0xaa, 0xa2, 0x05, 0xa0, 0x03,
                    0x4f, 0x01, 0xbb,
                ]);
            }
        }
        response.extend_from_slice(&[0x90, 0x00]);
        Ok(())
    }
}

fn decode_hex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
        .collect()
}

fn decode_historical_package(input: &str, trailing_padding: &str) -> Vec<u8> {
    // Go LoadBoundProfilePackage calls TLV.ReadFrom over a base64 decoder.
    // ReadFrom consumes one length-bounded TLV, without draining its reader.
    // Fixtures 2/4 have exactly two surplus '=' after that complete package.
    // Bound the prefix by the BER header, strictly decode it, and assert the
    // exact historical suffix rather than accepting arbitrary trailing data.
    let encoded: Vec<u8> = input
        .bytes()
        .filter(|byte| !matches!(byte, b'\r' | b'\n'))
        .collect();
    let prefix = STANDARD.decode(&encoded[..12]).unwrap();
    assert_eq!(&prefix[..2], &[0xbf, 0x36]);
    let length_octets = usize::from(prefix[2] & 0x7f);
    assert_ne!(prefix[2] & 0x80, 0);
    assert!((1..=4).contains(&length_octets));
    let header_length = 3 + length_octets;
    let value_length = prefix[3..header_length]
        .iter()
        .fold(0_usize, |length, byte| (length << 8) | usize::from(*byte));
    let package_length = header_length + value_length;
    assert!(package_length <= proletarius_euicc::MAX_BOUND_PROFILE_PACKAGE_SIZE);
    let encoded_length = package_length.div_ceil(3) * 4;
    assert_eq!(&encoded[encoded_length..], trailing_padding.as_bytes());
    let package = STANDARD.decode(&encoded[..encoded_length]).unwrap();
    assert_eq!(package.len(), package_length);
    package
}

fn assert_native_segments(name: &str, package: &[u8], expected: &[Vec<u8>]) {
    assert!(!expected.is_empty());
    assert_eq!(expected.concat(), package, "{name}: oracle covers package");
    for mss in [254, 37] {
        let transport = SegmentRecorder {
            expected_count: expected.len(),
            segments: Vec::new(),
            pending: Vec::new(),
            chunk_index: 0,
        };
        let mut card = Euicc::new(transport, 0, mss).unwrap();
        let result = card
            .load_bound_profile_package(package)
            .unwrap_or_else(|error| panic!("{name}, MSS {mss}: {error:?}"));
        assert_eq!(result.transaction_id(), &[0xaa]);
        assert_eq!(result.isdp_aid(), &[0xbb]);
        let transport = card.transport_mut();
        assert!(transport.pending.is_empty());
        assert_eq!(
            transport.segments.len(),
            expected.len(),
            "{name}, MSS {mss}"
        );
        for (index, (actual, expected)) in transport.segments.iter().zip(expected).enumerate() {
            assert_eq!(actual, expected, "{name}, MSS {mss}, segment {index}");
        }
    }
}

#[test]
fn historical_vendor_bound_profile_packages_match_literal_segment_files() {
    for (name, input, expected, trailing_padding) in [
        (
            "Infineon",
            include_str!("../testdata/bpp/bpp@1.txt"),
            include_str!("../testdata/bpp/sbpp@1.txt"),
            "",
        ),
        (
            "Redtea Mobile",
            include_str!("../testdata/bpp/bpp@2.txt"),
            include_str!("../testdata/bpp/sbpp@2.txt"),
            "==",
        ),
        (
            "Tigo",
            include_str!("../testdata/bpp/bpp@3.txt"),
            include_str!("../testdata/bpp/sbpp@3.txt"),
            "",
        ),
        (
            "Tele2",
            include_str!("../testdata/bpp/bpp@4.txt"),
            include_str!("../testdata/bpp/sbpp@4.txt"),
            "==",
        ),
    ] {
        let package = decode_historical_package(input, trailing_padding);
        let expected: Vec<Vec<u8>> = expected
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .map(decode_hex)
            .collect();
        assert_native_segments(name, &package, &expected);
    }
}

#[test]
fn historical_sequence_of_87_splits_into_seven_literal_segments() {
    // Input/outputs transcribed from TestSegmentedBoundProfilePackageSplitsSequenceOf87.
    let package = [
        0xbf, 0x36, 0x15, 0xbf, 0x23, 0x00, 0xa0, 0x06, 0x87, 0x01, 0x01, 0x87, 0x01, 0x02, 0xa1,
        0x03, 0x88, 0x01, 0x03, 0xa3, 0x03, 0x86, 0x01, 0x04,
    ];
    let expected = [
        vec![0xbf, 0x36, 0x15, 0xbf, 0x23, 0x00],
        vec![0xa0, 0x06, 0x87, 0x01, 0x01],
        vec![0x87, 0x01, 0x02],
        vec![0xa1, 0x03],
        vec![0x88, 0x01, 0x03],
        vec![0xa3, 0x03],
        vec![0x86, 0x01, 0x04],
    ];
    assert_native_segments("sequenceOf87", &package, &expected);
}
