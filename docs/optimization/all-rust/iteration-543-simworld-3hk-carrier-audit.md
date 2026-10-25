# Iteration 543 — SIM World / 3HK carrier identity and upstream audit

2026-10-18. The inserted SIM World profile is a 3HK subscription on home PLMN
454/03; its display name does not define a separate carrier template. Switch's
existing selector resolves generic 454/03 to `apple_hutchison_hk`, applies the
user-supplied `3_hk.json` overlay only to that selection, and preserves more
specific MVNO matches. The local iOS project's 3HK overlay has the same
protocol parameters; the only JSON differences are Switch-only `device: {}`
and `template_level: user` metadata. The iOS project was read only.

Read-only comparison against upstream `iniwex5/ios-carrier-profiles` main at
`09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25` found an identical 454/03
selector and identical Hutchison HK `common.ims` subtree. Upstream removed two
`default_ip_stack` keys from Hutchison's Apple APN list; those keys are not
consumed by Switch's VoWiFi runtime. Therefore changing the 3HK ePDG, IKE,
IMS, or proxy values to chase this revision would have no supported benefit.
The current 3HK runtime remains `wlan.three.com.hk` with the existing user
overlay and custom ePDG/port precedence.

The newer upstream corpus is broader: 676 profiles and 853 selectors versus
the pinned 685/855, with 33 existing profiles' `common.ims` subtrees changed,
four profiles added, and thirteen removed. A full refresh needs a new locked
snapshot and regenerated selection cases, with deliberate review of the
historical independent Go parameter oracle. It is separate from repairing
the current SIM World session, whose most recent real-card result is IKE/EAP
and CHILD SA success followed by IMS TCP timeout and ESP 8 sent / 0 received.
No runtime carrier registry was partially replaced in this iteration.

One actual display error was fixed: the modem serving-network name for PLMN
454/03 was incorrectly `电讯盈科`; it now reports `3 HK`. This is a display
mapping only and does not alter SIM home-PLMN selection or radio behavior.
The focused Rust 3HK tests and operator-name test pass. No card switching,
SMS, call, proxy-policy mutation, or DMG was performed.
