# Iteration 551 — consume the carrier's IMS IPsec policy

2026-10-18. Implemented and compiled the source-policy mapping. The focused
checks below passed after the hardware A/B comparison completed. No cargo
builds or tests were run during that comparison; it used frozen Release
binaries.

## Problem and runtime contract

The published carrier field `ims.register.use_ipsec` already reached
`RuntimeCarrierProfile.use_ipsec`, but the native IMS builder ignored it.
Sprint ISIM and Unitel MN newly publish `false` in the refreshed upstream
source. The local iOS engine explicitly maps `UseIPSec == false` to a disabled
sec-agree template at `Clarivate_IOS/mobilecore/engine.go:400`.

Rust instead retained its default `SecAgreeMode::Auto`: the initial REGISTER
advertises `Security-Client` and sends mandatory `Require: sec-agree` and
`Proxy-Require: sec-agree`, with the generic AKA bootstrap Authorization.
Auto permits a plain authenticated retry when a 401/407 has no Security-Server,
but it does not remove the requirement after an initial rejection such as
420. Thus a P-CSCF that rejects that required extension cannot complete this
path. No live Sprint/Unitel transaction establishes their actual response.

This policy controls the additional IMS association inside the authenticated
SWu/ePDG tunnel. It does not disable the outer IKE/ESP tunnel or its certificate
validation.

## Change and precedence

`native_ims_register_profile_with_home_domain` now reads the optional typed
`/ims/register/use_ipsec` value. An explicit `false` supplies
`SecAgreeMode::Disabled` as the carrier base in `resolve_template`. Missing,
null and true values retain the existing base policy. Non-boolean values are
rejected as invalid templates rather than interpreted as false.

The existing 3HK user template and then explicit
`ims_register_template.sec_agree_mode` overrides apply after this base. A user
can therefore explicitly select Auto or Required. Strict offer validation
remains available and enabled for those overrides; changing the carrier base
does not weaken an explicitly enabled session's offer admission.

Existing Disabled handling owns the wire change: no initial Security-Client,
Security-Verify, mandatory sec-agree Require/Proxy-Require, or generic bootstrap
Authorization. Optional Supported-header contents retain their existing
template policy. The 3HK EAP-direct bootstrap and its user template remain
unchanged. No registration tracker, security installer, transport, carrier
data/registry or certificate code was changed.

The mapping is a carrier-field contract rather than a two-operator whitelist.
The prior historical-input replay contains eleven false-projection rows
across five profile IDs: Vodafone HU, Sprint ISIM, Unitel MN, Smart PH and Tigo
CO. Sprint's two home PLMNs are included. The three other profiles already
published false before the latest refresh and need the same source-policy
consumption. All five IDs are represented in the new wire tests.

## Focused verification

Five new tests in `ims_profile.rs` cover:

- Real selectors for all five published false profiles, including Sprint
  310/120 and 312/530 with GID2 11, and absence of the initial mandatory
  security/bootstrap headers.
- Exact initial-wire equivalence for absent, null and true values, retaining
  Auto and its existing offer headers.
- Explicit session Auto/Required precedence over carrier false, retaining
  strict offer validation.
- Rejection of string, numeric, array and object values for the boolean field.
- Exact 3HK initial-wire preservation when the source field is false or true,
  and explicit session Required precedence over its supplied user template.

Verified with `SWITCH_BUILD_DIR` and `SWITCH_DATA_DIR` set inside the Switch
workspace:

- All five new source-policy tests pass.
- All 27 IMS-profile tests pass, including those five and the existing 3HK
  EAP-direct, protected REGISTER/Security-Verify, CTExcel, giffgaff and 3UK cases.
- All eleven tests across seven carrier integration targets pass, including
  2,726 historical selection decisions, original 2,726 Go parameter and 2,730
  proposal oracles, current source/export determinism and current offer audit.
- `rustfmt` parsed and formatted the edited source. Cargo test compiled the
  affected library and test binaries successfully.

Commands:

```sh
export SWITCH_BUILD_DIR=/Users/StatTrak/Desktop/Switch_MAC/build
export SWITCH_DATA_DIR=/Users/StatTrak/Desktop/Switch_MAC/build/runtime
cargo test --offline --manifest-path rust/Cargo.toml -p proletarius-vowifi --lib carrier_use_ipsec
cargo test --offline --manifest-path rust/Cargo.toml -p proletarius-vowifi --lib ims_profile::tests
cargo test --offline --manifest-path rust/Cargo.toml -p proletarius-vowifi \
  --test carrier_current_source --test carrier_export_golden \
  --test carrier_parameters_oracle --test carrier_effective_proposal_oracle \
  --test carrier_proposal_refresh_audit --test carrier_reference --test carrier_kaktus
```

The main task owns the subsequent full migration-gate rerun and review of its
source fingerprints. This note reports focused source/wire verification, not
new Sprint/Unitel operator-registration, SMS, calls or physical-card acceptance.
No carrier data, user overlay, registry, release binary or DMG was changed by
this iteration.
