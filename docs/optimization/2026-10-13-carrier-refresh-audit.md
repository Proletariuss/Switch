# Carrier registry refresh — 2026-10-13

## Outcome

Switch now embeds the latest checked `iniwex5/ios-carrier-profiles` `main` revision `cdba73e551810681f16b4cce0693c70e9e216c37`. The snapshot contains **685 carrier profiles and 855 PLMN selector manifests**, up from 683/853 at `e560437`. The SHA-256 lock for all 1,544 upstream files is recorded in [the pinned manifest](all-rust/carrier-reference-locks/ios-carrier-profiles-cdba73e551810681f16b4cce0693c70e9e216c37-manifest.json); older locks remain for reproducibility.

Clarivate_IOS was inspected read-only. Its embedded registry still identifies as `d46fc4ef5a3e4680443adf14748abd2a28361cd2`, with 683 profiles and 853 selectors. It is not the source of the newest carrier update. The newly refreshed public upstream is the authoritative source used for this import.

## What changed upstream

- Added generic carrier profiles/selectors for **Lebara UK (234/87)** and **DITO Philippines (515/66)**.
- Updated existing Apple bundle configuration data and added APN records across providers. The new/updated APN records are kept in the source snapshot; Switch's macOS VoWiFi runtime currently consumes the IMS subtree, not Apple APN provisioning settings.
- Added `iso` country metadata to selector entries. It is presentation/locale metadata and does not affect SIM selection. Go and Rust decoders ignore it safely while preserving it in the raw exported registry.
- The parsed `ims` subtrees for all 683 profiles already present in the prior Switch snapshot are structurally unchanged in this upstream revision. Therefore existing CTExcel, VDF, Loop, giffgaff, CMLink and Three UK VoWiFi parameters did not change in this refresh.

## Requested operator boundaries

| Operator | Selection basis | Current source result | What this refresh proves |
|---|---|---|---|
| CTExcel UK | Home PLMN 234/33 | Orange UK profile | Data selection and ePDG/template expansion only |
| Vodafone UK | Home PLMN 234/15; more-specific SIM selectors take precedence | Vodafone UK profile for generic 234/15 | Data selection and template expansion only |
| giffgaff | 234/10 with published GID1 selector `508FFFFF` | O2/giffgaff profile | Data selection; missing GID does not guess the MVNO |
| Loop | Home PLMN and card identifiers | Generic O2 fallback only when no constrained selector matches | No brand-only Loop profile was added upstream |
| CMLink | Home PLMN and card identifiers | Profile determined by published selectors; 234/30 resolves generic T-Mobile UK absent a specific match | No brand-only CMLink rule was invented |
| Three UK | 234/20 plus specific selector precedence | Hutchison UK profile | Data/proposal decoding only; certificate-authenticated IKE remains unsupported by the current native runtime |
| Lebara UK | 234/87 | New generic Lebara profile | Offline selector/ePDG expansion regression passed |
| DITO Philippines | 515/66 | New generic DITO profile | Offline selector/ePDG expansion regression passed; its published DH group 2 is not currently implemented by the Rust IKE runtime |

## Runtime and validation limits

The refreshed registry has been exported into the Rust embedded JSON snapshot; both Go and Rust use the same pinned data. Regression checks cover every published synthetic selector case plus explicit tests for the two new profiles. This verifies deterministic local selection, identity expansion, and configured ePDG values. It does **not** establish live ePDG reachability, SIM AKA success, IMS registration, calls, or SMS.

The runtime still fails closed where the published proposal is outside its implemented crypto set. In particular, Rust currently implements MODP group 14 for native carrier proposals; a profile advertising group 2 (including DITO) cannot be reported as operational until that cryptographic capability is implemented and reviewed. Certificate-required profiles also remain unavailable until peer certificate/AUTH validation is implemented. The registry update therefore broadens and refreshes carrier data without claiming universal carrier interoperability.

Custom ePDG and port precedence, proxy/DNS routing, user-owned 3HK template scope, and the existing exclusion of 9eSIM `6982` work are unchanged.
