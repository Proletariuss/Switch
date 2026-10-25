# Iteration 547 — complete carrier source refresh with retained selections

2026-10-18. Updated the preserved upstream carrier corpus to
`iniwex5/ios-carrier-profiles` commit
`09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25` and regenerated the Rust registry
and selection cases with the offline Rust exporter. Git objects at this exact
commit were used; the research checkout's HEAD is still an older commit and
was not treated as the latest source.

The new source lock records all 1,533 upstream files, including LICENSE,
README, index and inventory. The installed 676 profile YAML files and 853
selector YAML files, and MIT notice, match their exact upstream hashes. The
source lock is
`carrier-reference-locks/ios-carrier-profiles-09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25-manifest.json`.
Historical README/source records retain their original snapshot counts; this
iteration supersedes those descriptions for the current registry.

## Preserve existing supported selections

Simply replacing upstream data would have changed 41 of the historical 2,726
selection decisions. Some became unmatched or fell back to another operator;
other superficially similar replacements changed effective configuration:
Lebara IPv4 to IPv6, TMobile wholesale REGISTER contact parameters, and Nemont
ePDG/AES/DPD/media settings. Those replacements were not accepted as harmless
renames.

`data/carriers/compatibility/registry-overlay.json` retains 35 explicit raw
selector identities across 24 PLMNs, using 21 frozen profile objects. Every
profile and selector is checked against the unchanged historical registry;
the overlay also attributes original YAML paths and SHA-256 values to the
`cdba73e551810681f16b4cce0693c70e9e216c37` source lock. All 13 upstream-deleted
profiles remain available through this compatibility corpus. The exporter
replaces only these named raw selector rules, keeps the remaining upstream
rules, and refuses the overlay on a future upstream revision until reviewed.
The deployed resolver itself is unchanged.

The integrated registry contains **697 profile objects, 858 selector
manifests and 2,811 synthetic cases**, of which three retain deliberate
selector rejection. The current-source tests check every candidate's exact
profile reference and identity. All **2,726 historical selector decisions**
remain identical, including matched profile IDs and rejection behavior.
Every historical matched SIM also preserves request-level custom ePDG and
port precedence. The 43 affected historical cases retain their complete
effective runtime profiles, apart from current registry revision metadata.

The removed upstream profile filenames retained are:
`acs_us`, `apt_tw`, `chat_lte_us`, `crosswireless_lte_us`, `iwireless_us`, `mts_ca`, `nemont_lte_us`, `nwm_lte_us`, `panhandle_us`, `ptci_lte_us`, `sprint_virgin_isim_lte_us`, `telus_pcmobile_ca`, `tmobile_ting_us`.

## SIM World and protected carrier behavior

SIM World remains the user's 3HK subscription on home PLMN **454/03**. Its
generic selector resolves to `apple_hutchison_hk`, and the existing `3_hk.json`
user template remains scoped to that selection. More specific MVNO selectors
still win. The 454/03 upstream selector is unchanged, and Hutchison HK's
`common.ims` subtree is unchanged. Its upstream APN-list-only default-stack
removal does not change Switch's VoWiFi runtime.

`3_hk.json` was not edited. Its current SHA-256 is
`25a3279dfa6836a217dd714a54a024f882403df4e9c4593a819e5c9894527992`.
The local iOS project's corresponding JSON has identical protocol fields;
Switch alone adds `device: {}` and `template_level: user` metadata. The older
README's `e230b9...` hash is historical, not an instruction to rewrite the
current user template. The iOS project was read only.

The complete effective profiles for historical CTExcel/Orange UK, VDF,
giffgaff, 3UK, 3HK, and the PLMN mappings used by CMLink and Loop remain equal
to the pre-refresh profiles, apart from registry revision. Existing tests also
cover absent GID, exact MVNO matching, talkmobile precedence, the full 3HK
ordered proposal overlay, and YAML versus request custom ePDG/port precedence.
Display names do not infer subscriber entitlement or replace PLMN selectors.

## Explicitly adopted upstream changes

The raw upstream change surface is four added and thirteen removed profile
files, 401 changed existing profile documents, three added and five removed
selector files, and 27 changed existing selector documents. The compatibility
corpus above retains affected prior selections. The following 33 existing
profiles change their `common.ims` subtrees; this count does not include all
per-PLMN IMS changes:
`atn_bm`, `avantel_co`, `bhartiairtel_in`, `csl_hk`, `dish_ting_us`, `dito_ph`, `eastlink_ca`, `fastweb_it`, `idea_in`, `lebara_uk`, `mobilkom_at`, `movistar_ar`, `movistar_ec`, `movistar_pe`, `movistar_uy`, `primetel_cy`, `reliancejio_in`, `sfr_fr`, `smartfren_id`, `sprint_boost_isim_lte_us`, `sprint_isim_lte_us`, `telcel_mx`, `tmobile_boost_us`, `tmobile_metropcs_us`, `tmobile_ultramint_us`, `tmobile_us`, `uscellular_lte_us`, `verizon_charter_lte_us`, `verizon_comcast_lte_us`, `verizon_ting_lte_us`, `vodafone_in`, `wom_co`, `xplornet_ca`.

After compatibility retention, replay against the historical independent Go
parameter capture finds **2,648 unchanged effective-profile/rejection rows**
and **78 intentional differences across 30 profile IDs**. Revision metadata
and Go's omitted scalar zero values are normalized for this comparison.
All other fields and array order are compared. Six of these rows are display
name changes; the remaining changes are published settings adopted below.
This is a change audit, not an independent network-interoperability oracle
for new source values.

| Current profile ID | Historical rows changed | Changed effective fields |
| --- | ---: | --- |
| `apple_avantel_co` | 3 | `apn`, `early_media_needs_header`, `ip_stack` |
| `apple_cellc_za` | 2 | `child_dh_group`, `child_integrity`, `child_lifetime`, `disable_dpd`, `dpd_interval`, `early_media_needs_header`, `epdg_candidates`, `ike_dh_group`, `ike_integrity`, `ike_prf`, `support_p_early_media`, `template_esp_proposal_exact`, `template_ike_proposal_exact` |
| `apple_cellcom_lte_us` | 1 | `ip_stack` |
| `apple_chinatelecom_usim_mo` | 4 | `name` |
| `apple_cw_wi` | 10 | `ip_stack`, `support_p_early_media` |
| `apple_dialog_lk` | 2 | `child_dh_group`, `child_encryption`, `child_integrity`, `ike_dh_group`, `ike_encryption`, `ike_integrity`, `ike_prf` |
| `apple_dish_ting_us` | 2 | `child_encryption`, `child_lifetime`, `disable_dpd`, `dpd_interval`, `early_media_needs_header`, `epdg_candidates`, `ike_encryption`, `support_p_early_media`, `template_esp_proposal_exact`, `template_ike_proposal_exact` |
| `apple_dito_ph` | 2 | `eap_identity_template`, `local_identifier` |
| `apple_elisa_ee` | 2 | `ip_stack` |
| `apple_fastweb_it` | 3 | `register_contact_params` |
| `apple_lmt_lv` | 2 | `ike_encryption`, `ike_lifetime` |
| `apple_megafon_ru` | 2 | `child_dh_group`, `child_encryption`, `child_integrity`, `child_lifetime`, `disable_dpd`, `dpd_interval`, `early_media_needs_header`, `epdg_candidates`, `ike_dh_group`, `ike_encryption`, `ike_integrity`, `ike_prf`, `register_contact_params`, `support_p_early_media`, `template_esp_proposal_exact`, `template_ike_proposal_exact` |
| `apple_movistar_ar` | 5 | `child_encryption`, `disable_dpd`, `dpd_interval`, `epdg_candidates`, `template_esp_proposal_exact` |
| `apple_movistar_cr` | 2 | `early_media_needs_header`, `register_contact_params`, `support_p_early_media` |
| `apple_movistar_ec` | 2 | `name` |
| `apple_mts_ru` | 2 | `child_lifetime`, `disable_dpd`, `dpd_interval`, `early_media_needs_header`, `epdg_candidates`, `register_contact_params`, `support_p_early_media`, `template_esp_proposal_exact`, `template_ike_proposal_exact` |
| `apple_orange_mu` | 2 | `register_contact_params` |
| `apple_smart_ph` | 2 | `child_dh_group`, `child_encryption`, `child_integrity`, `ike_dh_group`, `ike_encryption`, `ike_integrity`, `ike_prf` |
| `apple_smartfren_id` | 6 | `ip_stack` |
| `apple_sprint_boost_isim_lte_us` | 2 | `ip_stack` |
| `apple_sprint_isim_lte_us` | 2 | `child_integrity`, `disable_dpd`, `dpd_interval`, `early_media_needs_header`, `epdg_candidates`, `ike_dh_group`, `ike_integrity`, `ike_prf`, `support_p_early_media`, `template_esp_proposal_exact`, `template_ike_proposal_exact`, `use_ipsec`, `validate_remote_certificate` |
| `apple_telefonica_tuenti_ar` | 2 | `child_encryption`, `disable_dpd`, `dpd_interval`, `epdg_candidates`, `ip_stack`, `register_contact_params`, `support_p_early_media`, `template_esp_proposal_exact` |
| `apple_tmn_pt` | 2 | `register_contact_params` |
| `apple_tmobile_boost_us` | 2 | `child_encryption`, `child_lifetime`, `disable_dpd`, `dpd_interval`, `early_media_needs_header`, `epdg_candidates`, `ike_encryption`, `support_p_early_media`, `template_esp_proposal_exact`, `template_ike_proposal_exact` |
| `apple_tmobile_us` | 2 | `epdg_candidates` |
| `apple_unitel_mn` | 2 | `early_media_needs_header`, `ip_stack`, `support_p_early_media`, `use_ipsec` |
| `apple_videotron_fizz_ca` | 1 | `child_dh_group`, `child_encryption`, `disable_dpd`, `dpd_interval`, `epdg_candidates`, `ike_dh_group`, `ike_encryption`, `ike_lifetime`, `template_esp_proposal_exact`, `template_ike_proposal_exact` |
| `apple_vimpelcom_ru` | 2 | `early_media_needs_header`, `register_contact_params`, `support_p_early_media` |
| `apple_vodafone_it` | 2 | `register_contact_params` |
| `apple_xplornet_ca` | 3 | `early_media_needs_header`, `ip_stack`, `support_p_early_media` |

The complete old/new values and synthetic SIM inputs are in
`../build/reports/iteration-547-carrier-effective-differences.json`, produced
from the unchanged independent historical Go capture and current Rust replay.

The current proposal audit compiles all **2,730** historical proposal rows.
**2,720 rows retain byte-identical IKE/ESP offers**. Ten rows (two each for
`apple_dialog_lk`, `apple_dish_ting_us`, `apple_mts_ru`, `apple_smart_ph`, and
`apple_tmobile_boost_us`) deliberately change both offers to exact source
proposals. MTS RU now uses IKE `aes128-sha256-prfsha256-modp2048` and ESP
`aes128-sha256`; the other four use the corresponding AES256 forms. The
regression test pins these carrier identities, row counts and new native
proposal lists, and requires every other old wire expectation to match.
The audit report is
`../build/reports/iteration-547-current-proposal-differences.json`.

## Security-sensitive refresh review

A post-refresh audit found one concrete newly introduced identity regression:
Verizon Ting's new upstream `local_identifier` uses `$impi_user`, while the
native expander currently substitutes only IMSI/MCC/MNC tokens and would
otherwise accept the literal unresolved token. Its two historical cases now
use the frozen historical profile/rules through compatibility. This raises
the retained-case count from the initially audited 41 to 43. A focused test
pins the historical identity template and asserts that the resulting EAP
identity and IKE IDi contain no unresolved `$`. The exact current upstream
YAML remains preserved. Other occurrences of `$impi_user` predate this
refresh and remain a separate existing implementation gap; no broad identity
or ISIM behavior was changed in this data update.

No historical effective-profile row changes certificate validation from
required to optional. Sprint ISIM changes `validate_remote_certificate` from
false/default to true with an explicit published certificate hostname; this
is a stricter request, subject to the existing runtime trust/capability gates.
DITO's newly explicit EAP username uses the same padded numeric home PLMN as
the former derived identity. The 3HK/UK protected paths remain unchanged.

Two upstream profiles now explicitly publish `ims.register.use_ipsec: false`:
Sprint ISIM (`310/120`, `312/530`) and Unitel MN (`428/88`). The compatibility
runtime projection therefore changes its boolean from true to false. However,
the native IMS REGISTER builder does not consume `RuntimeCarrierProfile.use_ipsec`
or this YAML field; it retains its existing template/override `SecAgreeMode`
policy (default `Auto` for these carriers). This is a source/native-runtime
parity gap, not evidence that this refresh disabled IMS wire protection or
that those operators' false setting is honored. A future implementation must
review the carrier security policy and test generated REGISTER/401 behavior;
blindly wiring the boolean to disable security is not established as correct.
Other raw IMS changes, including REGISTER expiry/contact fields, likewise do
not prove native wire behavior solely by appearing in a projected profile.

## Independent oracles and verification

The pre-refresh registry and 2,726 selector cases are preserved byte-for-byte
under `rust/testdata/carrier-history/`, with lengths and SHA-256 provenance.
The original Go parameter and proposal expectations were not regenerated or
edited. The historical tests replay the original registry, so upstream source
updates do not silently self-certify new expected protocol behavior. They
continue to verify **2,726 parameter cases and 2,730 proposal rows**. Current
source/export reproducibility and intentional upstream differences have
separate tests with accurately scoped names.

Verified locally:

- Six Python freshness/provenance regressions pass, including exact Git
  revision versus dirty checkout, additions/removals/content changes,
  source-lock mismatch/duplicates, symlink rejection, retained-source
  provenance and immutable historical-input hashes.
- Exact source/lock audit passes: 1,533 upstream source hashes; all installed
  YAML and license hashes match; retained profile/rule attribution matches
  the historical source. Report:
  `../build/reports/iteration-546-carrier-freshness.json`.
- All 49 Rust carrier-related library tests pass.
- Seven carrier integration targets pass (11 tests), including historical
  independent oracles, current-source continuity/protected effective profiles,
  source export determinism, the 25 fixed carrier boundaries, KAKTUS and the
  current proposal-change audit.

Commands:

```sh
python3 -m unittest discover -s scripts/tests -p 'test_audit_carrier_freshness.py'
python3 scripts/audit-carrier-freshness.py \
  --upstream-repo ../build/research/ios-carrier-profiles-main \
  --revision 09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25 \
  --lock docs/optimization/all-rust/carrier-reference-locks/ios-carrier-profiles-09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25-manifest.json
cargo test --offline --manifest-path rust/Cargo.toml -p proletarius-vowifi --lib carrier
cargo test --offline --manifest-path rust/Cargo.toml -p proletarius-vowifi \
  --test carrier_current_source --test carrier_export_golden \
  --test carrier_parameters_oracle --test carrier_effective_proposal_oracle \
  --test carrier_proposal_refresh_audit --test carrier_reference --test carrier_kaktus
```

No hardware/card mutation, operator registration, SMS, calls, proxy-policy
change or DMG was performed. No VoWiFi production protocol implementation
changed. These checks prove deterministic source import, retained selection,
configuration expansion and audited wire offers; they do not establish live
operator reachability, registration success, or full iOS-equivalent VoWiFi
interoperability. Existing runtime capability and certificate gates remain.
