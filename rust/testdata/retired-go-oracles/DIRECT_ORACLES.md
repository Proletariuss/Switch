# Independent projection and carrier expectations

These five files were exported from the retained Go implementation during
iteration 511. They contain synthetic inputs and independently computed Go
expected values. Ordinary Rust tests read the checked-in data without starting
Go, rebuilding an oracle, opening a modem, or accessing a physical card.

`direct-manifest.json` records SHA-256, byte and case counts, producer source
hashes, source revision, and exact one-time generation commands. Commands were
run from `src/backend`, with `scripts/workspace-env.sh`, the Xcode CC/SDKROOT,
and output under `Switch_MAC/build/reports/iteration-511-oracles`. The capture
used Go 1.26.7. The source revision alone does not describe the dirty migration
checkout; preserve the baseline and listed source hashes as historical evidence.

| File | Cases | Default Rust test |
|---|---:|---|
| `effective-proposals.json` | 2,730 | `carrier_effective_proposal_oracle::actual_go_effective_offers_match_native_for_all_registry_cases` |
| `usb-status.json` | 85 | `usb_at_status::tests::go_exported_projection_parity` |
| `radio.json` | 86 | `module_radio::tests::tests::legacy_go_radio_projection_and_failure_parity` |
| `http-methods.json` | 839 | `public_http::parity_tests::frozen_go_mux_method_and_head_wire_parity` |
| `carrier-parameters.json` | 2,726, including 3 rejections | `carrier_parameters_oracle::frozen_go_carrier_parameters_match_native_expansion` |

The proposal capture is byte-identical to the independently generated iteration
498 candidate. The HTTP capture is identical to iteration 500 for every case,
pattern, expected header, and body; only `go_version` changed from 1.26.3 to
1.26.7. Expected outputs were not generated or corrected using Rust output.

## Scope and intentional mappings

* Proposal bytes cover the Go executable DH14 IKE projection, renumbered after
  filtering, and all ESP offers with fixed SPI `00000001`. They do not claim
  equivalence to Go's original unfiltered DH15/DH2 advertisement. The file is
  an all-success corpus; consumers reject unexpected error rows rather than
  skipping them.
* USB expectations include all 16 public projection fields and the exact 8/14
  command sequence. This is frozen parser/projection behavior, not a hardware
  communication test.
* Radio status comes from a frozen Go projection. Its error/status mapping is
  reviewed literal API data. Rust compares real native failure stages against
  that mapping. The two existing diagnostic-text redactions remain explicit
  in the test; no state or failure expectation is discarded.
* HTTP expected wire behavior comes from Go's `http.ServeMux` with inert marker
  handlers. The capture uses a loopback test listener. It covers method, HEAD,
  Allow, representation length/body, and dispatch behavior; application state
  and device responses belong to separate native route tests.
* Carrier parameters are independently computed from Go `ExportSelectionCases`,
  `resolveEmbeddedProfile`, standard fallback, and user-template application.
  They were not copied from Rust's `carrier_replay`. The three 44055 ambiguous
  generic-selector cases remain rejected, not standard-profile fallbacks.
  Exact proposal flags are included even though Go `Profile` hides them from
  its ordinary JSON. Slice metadata preserves nil versus allocated-empty state.
  The consumer applies the old Go decoder's scalar-default equivalence to
  omitted zero/false/empty-string values while retaining arrays and raw `device`
  JSON values. It also exercises production custom-ePDG priority using the same
  synthetic IMSI substitution as the proposal fixture.

Every consumer asserts a fixed complete count; named corpora reject duplicate
names. Carrier cases compare all ordered input identities to the pinned selector
corpus. The manifest gate verifies byte hashes, sizes, and counts. Missing data
fails the build or test; these tests no longer carry `#[ignore]`.

Optional fixture-path environment variables remain available for an explicit
offline comparison (`SWITCH_PROPOSAL_ORACLE`, `SWITCH_USB_AT_STATUS_PARITY_FIXTURE`,
`SWITCH_RADIO_PARITY_FIXTURE`, `SWITCH_HTTP_METHOD_PARITY_FIXTURE`, and
`SWITCH_CARRIER_PARAMETERS_ORACLE`). They select data files only and never
activate a production compatibility backend. The normal suite requires none.

Future intentional behavior changes should update reviewed expectations with
independent evidence and provenance. Do not use the Rust implementation under
test to silently regenerate these historical expected results.
