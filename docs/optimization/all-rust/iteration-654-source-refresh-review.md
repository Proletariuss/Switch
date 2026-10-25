# Iteration 654 — reviewed current-source metadata refresh

This review binds the existing acceptance metadata to the current P5 audio FIFO and DJI module startup source. It does not change the frozen iteration-597 source graph, the iteration-652 source snapshot, requirement scope, route ownership, acceptance status, or an overall denominator. Hardware behavior, IMS, call audio, SMS, and DJI physical response are not certified by this metadata refresh.

## Source review

- `app/Sources/AudioBridge.swift` and `rust/crates/proletarius-audio-core/src/pcm_format.rs`: the P5 input FIFO front trim now keeps interleaved PCM frame boundaries; the Swift queue calls the Rust helper. Existing CoreAudio retirement fragments remain unique and were rebound to their current exact lines. This is source and targeted-test evidence, not an audible-call result.
- `rust/crates/proletariusd/src/module_startup.rs`: a definite serial-not-found plus definite native-USB-not-found result retries serial discovery, so a later enumerated CDC port can be seen. Once an owner is selected, startup continues with that owner. An uncertain USB release does not authorize another serial probe.
- `rust/crates/proletariusd/src/usb_at.rs`, `usb_at_transport.rs`, and `usb_at/native_startup.rs`: native USB candidate open runs under the retained serialized owner and commits management only after a successful session open; checked close and uncertain physical release remain fail-closed. The exact 12 USB dynamic-dispatch catalog source lines moved from 3969 to 3977. Method/path IDs, access policy, DTOs and Rust route ownership did not change. A real first-generation DJI AT response remains unverified.

The new `iteration-654-source-snapshot.json` contains 204 current files: the prior 203-file snapshot with five changed file hashes and the newly covered `usb_at/native_startup.rs`. The previous active maps were copied byte-for-byte to `entry-acceptance-map-iteration-652.json` and `runtime-owner-format-exit-map-iteration-652.json` before rebinding current facts. Seven historical runtime nodes now say `UNIQUE_EXACT_FRAGMENT_REBOUND` where their whole-file hashes differ, and the three AudioBridge current replacement nodes point to their unique current spans. Historical `source_graph`, `raw_sha256`, and frozen-cut references were not changed. The active maps remain non-scoring; all 115 requirement leaves and 21 scenario classes retain their prior status.

## Verification

- Manual census diff: only 12 USB dynamic-dispatch handler lines changed; four reviewed whole-file scope fingerprints changed. `--write-inventory` followed that review.
- `check-rust-migration-contracts.py --require-rust-only`: 294 method/path contracts, 294 Rust owner declarations, 0 Go owner declarations, 129 Swift HTTP call sites, 467 reviewed scopes; pass.
- `check-acceptance-ledger.py`: metadata consistent with zero errors; 294 API entries, 131 Swift catalog entries, 56 RT entries; no functional pass or denominator freeze.
- `python3 -m unittest scripts.tests.test_acceptance_ledger`: 61 tests passed.
- `preflight-source.sh`: version 3.46/build 346, Go backend exit, and recorded route/consumer census passed.

Targeted DJI startup and USB owner tests, and P5 audio tests, are maintained separately as software behavior evidence. No DMG was produced by this metadata step.
