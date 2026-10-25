# Carrier profile refresh audit — 2026-10-12

> Historical snapshot. Superseded by [the 2026-10-13 audit](2026-10-13-carrier-refresh-audit.md), which records the latest upstream revision and current file counts.

## Result

Switch's embedded Apple carrier registry is synchronized to `iniwex5/ios-carrier-profiles` revision `e5604370f93b863ee212dd3cdc5294979f11bca4`, the upstream `main` revision checked on 2026-10-13. Switch embeds 683 profiles and 853 selectors. The 2026-10-12 source refresh supplied the carrier-specific configuration; the 2026-10-13 update adds selector-level display names to 10 files. Those names are presentation metadata and do not affect SIM matching or VoWiFi configuration. A full SHA-256 lock is stored in [ios-carrier-profiles-e5604370f93b863ee212dd3cdc5294979f11bca4-manifest.json](all-rust/carrier-reference-locks/ios-carrier-profiles-e5604370f93b863ee212dd3cdc5294979f11bca4-manifest.json); the previous d46fc4e lock remains available for reproducibility.

Clarivate_IOS was checked read-only on 2026-10-13. Its `mobilecore/internal/vowifi/carrier/data` still contains 683 profiles and 853 selectors from `d46fc4e`; it does not yet include the newer selector display fields. The newly updated iOS files inspected for release 626 concern B0/B1 reader lifecycle, UI, and app stability rather than carrier templates. They are platform-specific and do not add reusable macOS VoWiFi carrier configuration. Switch now carries the upstream e560437 selector metadata independently. Its `3_hk.json` remains app-owned, not part of the Apple carrier-profile refresh; the Clarivate and Switch copies have different schema bookkeeping and hashes, so neither was copied over the other.

Clarivate cross-check on 2026-10-13: the 683 profile YAML files match Switch byte-for-byte. Ten selector files differ only because Switch already includes e560437's `carrier_name` presentation metadata; the selector conditions and carrier configuration are unchanged. Clarivate's `3_hk.json` explicitly sets `include_pani=false` and `include_pani_authenticated=false`; Switch now records both values explicitly. Go already emitted the same behavior through zero-value booleans. The gated Rust IMS resolver now reads `include_pani_authenticated` from this user template only for 454/03, while preserving explicit carrier-override precedence and leaving 454/04 unchanged. Offline regression coverage passes; this does not claim SIMworld registration or production Rust IMS enablement.

## Requested operators

| Card/brand | Selection evidence | Current configuration result | Remaining limit |
|---|---|---|---|
| CTExcel UK | Home PLMN `234/33` | `apple_orange_uk`; published ePDG and IPv6, IKE/ESP proposal, DPD and child lifetime are present | Static config and SIP-wire tests only; no live operator registration in this audit |
| Vodafone UK (VDF) | Home PLMN `234/15`; `GID1=C1` selects Talkmobile instead | Generic `apple_vodafone_uk` with the published UK ePDG and IPv4 profile; a Rust regression test now checks this and Talkmobile precedence | A carrier label alone cannot override a specific SIM selector |
| giffgaff | `234/10` plus published `GID1` prefix `508FFFFF` | `apple_o2_giffgaff_uk`; explicit ePDG, IPv6 and exact IKE/ESP proposals | SPN `giffgaff` alone is insufficient and intentionally does not invent the constrained selector |
| Loop | No dedicated Loop brand selector in the source revision | The actual PLMN and SIM identifiers select a profile; `234/10` with no matching GID falls back to generic O2 | Do not map Loop from SPN/brand alone; confirm the card's MCC/MNC/GID/ICCID first |
| CMLink | No dedicated CMLink brand selector in the source revision | For `234/30`, generic `apple_tmobile_uk` is selected; other home PLMNs resolve independently | The brand name alone is insufficient; real SIM identity decides the profile |
| Three UK | Home PLMN `234/20`; specific GID selectors keep precedence | `apple_hutchison_uk` publishes TCP IMS transport, separate anonymous IKE ID/permanent EAP username, AES-256/SHA-256/DH14 IKE and AES-128/SHA-256 ESP | The source requires remote certificate validation. Rust currently parses/stores this flag but the SWu runtime does not validate the ePDG certificate/AUTH payload, so full 3UK compatibility is not complete |

## Validation boundary

Go carrier package tests and Rust VoWiFi carrier/runtime-profile tests exercise source selection, parameter expansion, custom ePDG precedence, and protocol/header fixtures. These are offline checks. They do not prove ePDG reachability, valid SIM AKA, certificate trust, IMS registration, calls, SMS, or roaming behavior on a live card.

No carrier profile is force-selected by marketing name. Custom ePDG/port remains first in precedence, followed by a matched profile and then the 3GPP-derived hostname. The configured proxy/DNS path is unchanged.
