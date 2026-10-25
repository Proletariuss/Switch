# EVS WB 13.2 reference regression fixture

Synthetic one-second 16 kHz mono signed little-endian PCM: 430 Hz + 2900 Hz;
no captured speech or user data. Generated with the vendored, unmodified
TS 26.443 V16.3.0 command-line encoder/decoder sources, compiled on arm64.

Commands (disable file-only delay compensation to match streaming frames):

```
EVS_cod -q -no_delay_cmp -mime -max_band WB 13200 16 input.pcm reference.evs
EVS_dec -q -no_delay_cmp -mime 16 reference.evs decoded.pcm
```

The Go and Rust native adapters are checked byte-for-byte against both
reference outputs. Rust keeps an independent copy so this regression remains
available after the Go package is retired. This is a regression check, not a
claim of formal 3GPP conformance.

`amrwb1265.evs` and `amrwb1265.pcm` use the same commands with bitrate
12650. They cover the AMR-WB interoperable core independently of EVS Primary.
All nine AMR-WB rates are also roundtripped through both RFC 4867 wire formats.

Iteration 511 preserved the two AMR-WB files byte-for-byte from the former
`backend/internal/vowifi/imscore/testdata/evs/` directory; expected output was
not regenerated from the Rust implementation. Original oracle assertion:
`TestAMRWBReferenceAndAllModes` in `evs_codec_test.go`. The shared `input.pcm`
was already byte-identical in this directory.

| Reference file | Bytes | SHA-256 |
| --- | ---: | --- |
| `amrwb1265.evs` | 1666 | `8c1776d79fa69637fa5fa393648c8666100d5c862a98dc06f22d6ae56737f90a` |
| `amrwb1265.pcm` | 32000 | `d84180213aa376e8309436dbdad9424698405084e21cb99d072b0f6008a1364c` |

Reference algorithm provenance and notices remain in
`third_party/evs/NOTICE.md` and `readme.txt` at the repository root. The
relocation does not change or expand their licensing terms.

The default Rust tests in `src/evs.rs` now check all 50 AMR-WB 12.65 kbps
encoder frames against the frozen bitstream and feed that bitstream to a
separate native decoder, comparing every little-endian PCM byte. The decoded
reference contains 15,956 nonzero samples, minimum -7,286, maximum 7,549, and
sum of squared samples 159,764,719,564. These fixed signal facts are also
asserted. Native smoke coverage exercises all nine modes through both RTP
formats and a lost frame; it does not replace the independent golden checks.
On the shipping macOS arm64 target a default availability test fails when
the native archive is missing, so the golden tests cannot silently disappear
behind `switch_evs_available`. Other targets retain their existing codec
availability rules.
