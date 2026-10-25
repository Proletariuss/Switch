> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# 0.37 work in progress — 2026-09-05

Not a release and not completion of the Rust migration.

## Implemented and checked

- Follow-up UI: diagnostics prevents duplicate collections, clears stale results on failure and reports backend-unavailable instead of indefinitely implying loading. eSIM toolbar drops redundant card-type text and uses shared primary button/purple tint. Complete macOS 26 Release build and the full Rust workspace test suite passed after these changes; visual inspection is still pending.

- Reader voice SDP offers PCMA only. Incoming dynamic PCMA payload mappings are preserved; unoffered payload mappings in answers are rejected.
- Incoming answers do not invent telephone-event support. Audio SDP parsing no longer consumes a video media address.
- Reader audio encode/decode uses A-law. Uplink accepts 160-sample PCM frames and updates the local audio peer only after token validation.
- Rust archive loading accepts legacy Go `null` after clearing an archive.
- Shared compact button styles, section colors and simplified activity/sidebar and selected page controls.
- `go test ./internal/vowifi/imscore ./internal/vowifi/carrier`: passed.
- `cargo test --offline -p proletarius-store`: 5 passed.
- Swift view syntax parsing and complete macOS 26 Release Xcode build: passed.

## Remaining gates

- Real CTExcel IMS registration, PCMA call to 888, microphone/speaker/AirPods and DTMF validation. PCMA-only carrier acceptance is NOT established by unit tests.
- Finish remaining page review and visual inspection; current UI changes are not yet visually accepted.
- Migrate actual runtime ownership beyond the current Rust gateway; Go compatibility backend remains required.
- Native archive mutation must reconcile Go memory before UI cutover, or deleted messages may reappear.
- Synchronous archive locking/file IO now runs on blocking workers for both native SMS and call archive routes. All 27 Rust gateway tests passed, including a single-thread executor responsiveness regression.
- Remaining archive cutover: Go `allSMS` merges memory with disk, while `recordSMSFromDevice` writes both. Replacing deletion routes alone resurrects deleted records; migrate producer ownership/cache reconciliation before switching the UI.
- Verify latest upstream carrier revision before claiming template synchronization.
- Three complete cross-review/live cycles and final release packaging remain outstanding. No DMG generated for this iteration.

No proxy configuration changed. Use manually supplied `127.0.0.1:7897` for authorized live testing; do not introduce DNS bypass.

## Rust PCMA core

- Added allocation-free A-law sample/frame encode/decode to `proletarius-call`, with an explicit 8 kHz mono, 160 samples / 20 ms, little-endian PCM bridge contract.
- All 65,536 PCM values checked for monotonic decoded output and bounded quantization; all 256 codewords checked for roundtrip identity, plus segment boundaries and frame endianness.
- All 11 call-crate tests pass. This core is not yet wired into the live audio transport: Go still owns RTP and Swift owns CoreAudio.
- Standard reference: https://www.itu.int/rec/T-REC-G.711-198811-I/en

## Rust call-state review

- Reliable 101–199 responses now participate in PRACK handling; RSeq must be consecutive after the first accepted response, with duplicate/gap responses ignored without adopting their SDP.
- A validated reliable early answer can be reused for a final 2xx without SDP; cancellation still acknowledges reliable responses but does not start early media.
- `has_sdp` explicitly requires successful SDP validation, and the tracker is scoped to one dialog. Runtime integration must honor this contract rather than checking only for a nonempty body.
- All 14 call-crate tests and strict Clippy passed. Reference: https://www.rfc-editor.org/rfc/rfc3262.html

## Native reader session preparation

- Native hardware probe executed on this Mac on 2026-09-05: PC/SC returned `reader_count=0`; no connection or APDU was attempted. This is not a completed hardware cycle. Probe only supports exactly one available, nonbusy reader and uses exclusive acquisition plus SELECT MF, without printing card identity or response data.

## Carrier registry migration data

- Full parameter replay now passes for all 2,722 synthetic cases: Rust selection/recursive merge -> native wire response -> Go compatibility transport-model conversion equals the old embedded resolver's complete Profile object. Each accepted case also preserves a manually supplied custom ePDG.
- Generate replay from `rust`: `cargo run --offline -p proletarius-vowifi --example carrier_replay -- ../build/carrier-replay.json`. Run from `backend` with `PROLETARIUS_CARRIER_REPLAY` pointing at that absolute generated path: `go test ./internal/vowifi/carrier -run TestRustCarrierParameterReplay -v`. Without the explicit replay path this integration test skips; ordinary unit test success alone does not prove replay ran.

- Added deterministic synthetic selection replay exported from every pinned manifest's generic identity and selector conditions. All 2,722 cases match Go's embedded selection result (profile ID, raw selector, no-match or rejection). This is selection parity only, not exhaustive malformed-input coverage, parameter mapping equivalence or a real IMS registration cycle.
- Reproduce fixtures from `backend`: `go run ./cmd/export-carriers -cases -output ../rust/carrier-cases.json`; Rust test `pinned_registry_matches_go_synthetic_selection_cases` consumes them offline.

- Added an offline deterministic exporter preserving full pinned YAML documents, including generic/constrained selectors and common/PLMN/selector overrides.
- Export completeness test verifies 853 selectors, 683 profiles and the existing pinned revision. Runtime resolution still uses Go until a native resolver consumes this data and passes equivalence tests.
- Reproduce: from `backend`, run `go run ./cmd/export-carriers -output ../rust/carrier-registry.json`. No live SIM identifiers or network requests are involved.
- Native Rust carrier resolver now consumes the embedded JSON, preserves MCC/MNC widths, selects constrained/generic profiles, rejects ambiguous best matches, validates profile identity, and recursively applies PLMN/selector overlays while replacing arrays. Missing `common` is accepted because some pinned bundles contain PLMN overrides only.
- Five tests passed for complete registry loading, giffgaff GID selection, missing-GID fallback, CTExcel PLMN configuration and merge semantics. Strict crate Clippy passed before the final two test additions. Gateway/profile mapping and live integration remain outstanding; custom ePDG handling has not been changed.
- Added authenticated native `POST /api/v2/carrier/resolve` with a 4 KB request limit and cached registry loading off async workers. It returns matched configuration or a no-match result independently of Go recovery. All 28 gateway tests and strict gateway Clippy passed; real VoWiFi callers still use Go profile resolution until mapping/cutover tests are finished.
- Added a five-second whole-body read deadline, decoder tests for malformed/oversized requests, and an equal-specificity conflicting-profile fixture. Six resolver tests and two handler/decoder tests passed; strict gateway Clippy passed. Socket-level deadline/auth tests and live caller cutover remain outstanding.
- VoWiFi start now uses the native carrier endpoint when launched by the Rust gateway (gateway supplies its local socket path). A bounded authenticated Unix client maps the already-selected/merged result into the temporary Go transport model; native errors stop startup rather than selecting a Go fallback. Direct standalone Go remains compatible. Status/reader-info preview paths still need the same cutover.
- Local Unix test verifies token/path/method, native parameter mapping and no fallback after native rejection. Existing reader VoWiFi/carrier configuration tests pass. Actual gateway-to-live-reader registration remains unverified; no proxy or ePDG override logic changed.
- WiFi Calling status and reader-info previews now also use `resolveRuntimeCarrier`. A failed native lookup does not dereference a missing profile or silently generate a Go template. Reader-info exposes an optional `carrier_config_error`; existing active tunnel status remains available. Reader-related Go regression tests passed. Swift presentation of the new reader-info error field is still pending.
- Swift now decodes and displays `carrier_config_error` by the reader ePDG controls, clears it on disconnect, and clears a stale operator name when the refreshed identity has none. Latest complete macOS 26 Release integration build passed with these changes. No runtime installation, visual acceptance or live call/SMS success is implied by this build.
- Native carrier client now accepts a caller context; WiFi Calling status requests propagate cancellation. Pre-canceled requests and cancellation after a local Unix test server receives the request are covered. This change does not alter live tunnel-stop ownership or claim cancellation parity for the remaining startup path.

- Added an exclusive PC/SC session implementing the Rust SIM APDU transport, with bounded short APDU buffers and explicit close error reporting. It is not yet constructed by gateway routes; Go retains sole live ownership.
- Rust now owns bounded SOCKS5 no-auth and UDP ASSOCIATE capability probing through `/api/v2/vowifi/proxy-probe`. The Go compatibility runtime consumes that result for proxy checks and transport admission; live route construction and IKE/ESP/IMS ownership remain unchanged.
- Rust discovery treats `NoReadersAvailable` as an empty inventory, so removing the final reader is not reported as an enumeration failure.
- Four no-hardware reader tests pass. Hardware handoff, real APDU exchanges and AKA remain to be validated before routing reader actions to this session.
- Rust GET RESPONSE now retains the original logical channel instead of always using channel zero; initial responses also obey the 4096-byte data limit, and malformed short APDU framing is rejected before transport.
- Added bounded 6C Le correction for actual case-2/case-4 length fields, including GET RESPONSE, without changing case-3 AKA payload data. GET RESPONSE and length corrections share a maximum of eight follow-up transmissions. Nine SIM and four reader tests pass; strict SIM Clippy passes. Extended APDU and secure-message handling are not implemented by this short-APDU helper.

## SMS producer safety follow-up (details)

- Confirmed module adoption cleared SM/ME even when archive persistence failed. Added a checked persistence result and stop before device clearing on archive failure/unavailable archive.
- Duplicate messages already persisted are a successful no-op, distinct from failed writes. Added corruption, invalid lock target and duplicate regression cases.
- This is an immediate safety correction in the still-required compatibility producer, not a claim that SMS producer ownership has migrated to Rust.

## Native SMS envelope cutover (2026-09-06)

- The explicit user-requested 0.37 TEST DMG was built from the formal tree before this cutover; it does not contain these later SMS changes. The formal tree retains version 0.37/build 37; staging version metadata must not overwrite it.
- Reader MO RP-DATA encoding now calls authenticated Rust `/api/v2/sms/encode-rpdu` over the gateway Unix socket. Encoding completes before IMS transmission; gateway failures do not silently fall back. Standalone Go compatibility remains until the transport cutover.
- Preserved prior SMSC normalization and numeric JSON octet arrays. Rust SMS/gateway tests: 38 passed, strict Clippy passed. Go NativeSMS/RuntimeCarrier/ReaderVoWiFi regression selection passed.
- Added cancellation before either encoding backend, and validation of RP envelope framing, TPDU length and payload identity. Regression tests cover canceled requests, corrupted headers/payload, all truncations and trailing bytes.
- Incoming RP parsing/ACK, TPDU text encoding/decoding and live IMS transport still run in Go. No real SMS was sent in this validation; full Rust removal and hardware loop gates remain open.

## SMS text cutover follow-up

- Reader outgoing TPDU preparation now calls Rust `/api/v2/sms/prepare-tpdu`, including GSM-7 default/extension alphabet, bit packing, UTF-16 and concatenation framing. Incoming RP envelope parsing also uses Rust; incoming TPDU text decoding and actual IMS transport remain Go.
- Incoming SMS persistence errors now propagate to the transport; RP-ACK is sent only after successful archival. An offline SMS-DELIVER fixture checks this ordering. No carrier retransmission behavior has been verified.
- Rust SMS core has 17 passing tests. The offline `submit_fixture` example is consumed by `TestNativeSMSSubmitParity` when `PROLETARIUS_SUBMIT_FIXTURE` points to the compiled example. Thirty recipient/text combinations matched the existing Go implementation byte-for-byte, normalizing only the randomized concatenation reference. This test skips without the environment variable; ordinary Go test success alone does not prove parity.
- Latest hardware probe enumerated zero readers. No live SMS/call cycle was performed. These changes postdate the 0.37 TEST DMG and are not included in that previously delivered image.
