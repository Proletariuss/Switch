# Iteration 054 — NekokoLPA2 / 9eSIM authentication compatibility

Date: 2026-10-02  
Release candidate: Switch 2.74 (build 274)
Scope: Switch-local eSIM download compatibility; no profile was downloaded in this iteration.

## Source review and findings

- Reviewed NekokoLPA2 v2.2.6+588 (source revision `013eadad7037248531f1b1532ebf344390d57087`), especially `profile_manager.dart`, `imei_codec.dart`, and the CCID adapter. Its profile download uses the standard SGP.22 `AuthenticateServer` exchange. The request carries TAC, device capabilities, an optional full IMEI, and matching ID; IMEI is GSM-swapped BCD. No separate 9eSIM profile-decryption key or app-side BPP decryption routine was found.
- The release publishes Android packages with a `9eSIM Signature` identity as well as community/open signatures. That signer identity is an Android card-access authorization detail, separate from the SGP.22 profile-protection exchange; it cannot be transplanted as a macOS application signature or inferred from `6982` alone.
- A `6982` at `AuthenticateServer` is returned by the eUICC before `LoadBoundProfilePackage` begins. It indicates a card-side security-status refusal, not proof that the profile package's encryption failed. The exact cause still requires comparing the selected AID and APDU/session context on the same reader/card.
- Switch already follows the compared defaults: it sends TAC `35383741` when no identity is supplied, encodes an explicitly supplied 15-digit IMEI in the same swapped-BCD form, prioritizes SE1 for identified 9eSIM cards, and uses the existing external-reader STORE DATA chunking path. The Go request encoder has golden tests for both TAC-only and TAC-plus-IMEI encodings.
- NekokoLPA2 is MIT licensed. This iteration adapts its optional device-identity behavior through Switch's existing API instead of embedding its Flutter runtime or copying Android signing material. No Nekoko code was copied, so no new third-party notice is required.

## Switch change

- Added a per-download optional TAC/IMEI field. Leaving it blank retains Switch's Nekoko-compatible TAC-only default. Users can enter an 8-digit TAC or a valid 15-digit IMEI when the service provider requires that identity; the backend validates the value and does not persist it.
- Added an in-app note that a full IMEI is included in the eUICC `AuthenticateServer` request and should only be supplied when required.

## Validation and remaining evidence

- Validation: Swift Release build for 2.74/build 274, strict app-signature check, `git diff --check`, and focused Go tests for eUICC request encoding and download manager behavior.
- Not verified: a fresh physical 9eSIM download with this build. The default identity already matches NekokoLPA2's default TAC, so this UI option can only change the result if NekokoLPA2 was configured to send a different TAC or full IMEI. If the same `6982` persists with matching inputs, compare the exact APDU bytes and selected reader/AID before adding any brand-specific command.

## References

- [NekokoLPA2 v2.2.6+588 release](https://github.com/iebb/NekokoLPA2/releases)
- [NekokoLPA2 profile manager](https://github.com/iebb/NekokoLPA2/blob/013eadad7037248531f1b1532ebf344390d57087/lib/logic/profile_manager.dart)
- [NekokoLPA2 IMEI codec](https://github.com/iebb/NekokoLPA2/blob/013eadad7037248531f1b1532ebf344390d57087/lib/utils/imei_codec.dart)
- [NekokoLPA2 CCID adapter](https://github.com/iebb/NekokoLPA2/blob/013eadad7037248531f1b1532ebf344390d57087/lib/adapter/ccid/ccid_adapter.dart)
- [GSMA SGP.22 v2.2.2](https://www.gsma.com/solutions-and-impact/technologies/esim/gsma_resources/sgp-22-v2-2-2/)
- [NekokoLPA2 MIT license](https://github.com/iebb/NekokoLPA2/blob/013eadad7037248531f1b1532ebf344390d57087/LICENSE)
