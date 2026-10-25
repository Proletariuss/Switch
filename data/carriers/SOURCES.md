# Carrier data provenance

**Current source revision (2026-10-18):** The upstream YAML corpus is now
`09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25`, with the explicit historical
compatibility overlay described in [README.md](README.md) and
[Iteration 547](../../docs/optimization/all-rust/iteration-547-carrier-refresh.md).
The `cdba73e` counts, hashes, and limitations below are the preserved
historical record, not the current inventory. The current 3HK user-template
hash is `25a3279dfa6836a217dd714a54a024f882403df4e9c4593a819e5c9894527992`.

The 855 selectors and 685 profiles are pinned to
[`iniwex5/ios-carrier-profiles@cdba73e551810681f16b4cce0693c70e9e216c37`](https://github.com/iniwex5/ios-carrier-profiles/tree/cdba73e551810681f16b4cce0693c70e9e216c37),
verified against upstream `main` on 2026-10-13. The registry contains 685
profiles and 855 selectors. The latest commit adds DITO Philippines and Lebara
UK profiles, expands Apple carrier APN settings, and adds selector country-code
metadata. Existing profile VoWiFi `ims` configuration trees are unchanged from
`e560437`; the new APN fields are retained as source data but are not consumed
by the macOS VoWiFi runtime. Clarivate_IOS was checked read-only and remains at
`d46fc4e`, so it does not provide the newest carrier-data delta.
The upstream project identifies these files as MIT licensed. Preserve the
copyright and terms in [ios-carrier-profiles-MIT.txt](ios-carrier-profiles-MIT.txt).

The 2026-10-05 audit compared the existing effective first ePDG against MIT
upstream revision `1751fd3d3629927ae8499aa9d682130c025a965d` and reviewed
VoCat revision `0dec0cbfa3445695e44a5108d88db399f639fccb` as a research reference.
It did not replace any selector/profile YAML or import VoCat runtime/configuration.
VoCat's root license is Research & Evaluation; public availability does not
turn that code or its configuration collection into MIT-licensed material.

See `docs/optimization/2026-10-05-vocat-carrier-integration.md` for scoped
compatibility findings, reproducible offline checks and pending runtime work.
The 2026-10-12 refresh updated 254 existing profile files (971 added / 639
removed lines). The 2026-10-13 refresh adds display names to selectors; they
are returned as profile labels only and never participate in selection.
Profile source metadata includes explicit ePDG addresses and, for some
carriers, IKE username templates and IMS transport choices. Those values are
parsed separately from IKE local identifiers. On macOS, the SWu runtime
verifies a presented responder certificate chain and IKE AUTH signature using
system trust without network certificate fetch. A profile with
`validate_remote_certificate` requires the certificate path; otherwise a
presented certificate is still verified when used. These source parameters
and offline checks do not establish real-carrier interoperability.

The 685 profile count is a configuration inventory, not a claim of tested
VoWiFi service on every operator, country, card or subscription. The full
2026-10-13 source-file lock is recorded in
[the cdba73e manifest](../../docs/optimization/all-rust/carrier-reference-locks/ios-carrier-profiles-cdba73e551810681f16b4cce0693c70e9e216c37-manifest.json).
The operator selection and runtime compatibility boundaries are summarized in
[the historical carrier refresh audit](../../docs/optimization/2026-10-13-carrier-refresh-audit.md).

2026-10-18 check: upstream `main` is
[`09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25`](https://github.com/iniwex5/ios-carrier-profiles/tree/09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25).
The 454/03 selector and Hutchison HK `ims` subtree are unchanged from this
pinned snapshot. Its two removed `default_ip_stack` keys are Apple APN metadata,
not inputs to Switch's VoWiFi runtime. This check does not change the pinned
runtime registry or claim that the other upstream changes have been integrated;
see [the scoped audit](../../docs/optimization/all-rust/iteration-543-simworld-3hk-carrier-audit.md).
This was the pre-refresh check; the subsequent Iteration 547 integrated and
audited the `09b8ae86` corpus with an explicit compatibility overlay.

## User template overlay (2026-10-07)

`user_templates/3_hk.json` is the user-provided file, copied unchanged. SHA-256:
`e230b9b819f319b6ab0c88878879bdac78c1653bc733e80215c98a7738196e84`.
This hash records the 2026-10-07 file before Switch-only `device` and
`template_level` metadata were added; the current file hash appears above.
The protocol fields remain identical to the local iOS project's 3HK template.
Its scope is the already selected `apple_hutchison_hk` profile on home PLMN
454/03 only; constrained MVNO profiles and 454/04 keep their original settings.
This does not alter or relicense the pinned Apple YAML/registry. The user
provided the configuration as input; no VoCat runtime code is copied for it.
Entitlement URL is retained as metadata and is not contacted by template loading.

The user identified [voorz/vohive-release](https://github.com/voorz/vohive-release)
as the exporter. Protocol behavior was cross-checked against its core dependency
[`voorz/vowifi-core` v1.4.5](https://github.com/voorz/vowifi-core/tree/eddbf72e6f722fdd2b10a911ee5212d158dce784)
and its SWu dependency v0.3.8. These are behavioral references, not imported
runtime implementations. Switch implements the required behavior through its
existing protocol libraries. Upstream profile fields absent from the supplied
JSON (including options that disable EAP MAC validation) are not imported.
