# Iteration 545 — isolated old-binary data read

2026-10-18. P6 downgrade compatibility was exercised against the actual
Switch 3.42 app bundle, which contains a Rust gateway and a Go child. Starting
the Go child alone is not a valid downgrade test: its startup requires the old
gateway's private USB configuration endpoint. The two bundled executables were
started together with a short Unix socket path, a synthetic token and a fresh
isolated data root. No installed application data or physical card state was
used.

The input was copied from the current Rust daemon's independent recovery test:
SMS archive, device send ledger, N-standby store and operations journal. The
source archive contains two synthetic messages. The old pair reached `ready`
with its Go child connected and returned HTTP 200 for all three read checks:
two archive messages including `fixture-accepted`, two N-standby messages, and
the interrupted send intent as `unknown` with `can_submit=false`. SHA-256 of
all four copied input files was unchanged after the run. The current Rust
independent recovery test also passed. The sanitized observations are in
`build/reports/iteration-545-legacy-downgrade-current-sanitized.json`.

The old 3.42 pair exited with code 1 on SIGTERM in this module-free test
environment because its call-mode shutdown demanded an unavailable USB ADB
interface. This is a separate legacy shutdown defect; it does not contradict
the read result, but prevents claiming complete downgrade acceptance. This
test also does not cover an installed-app rollback, older UI behavior, live
hardware, write-after-downgrade or every persisted data type. Those P6 checks
remain open.

The same turn rechecked carrier input: `iniwex5/ios-carrier-profiles` main was
still `09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25`. Its 454/03 selector
exactly matches Switch. Its Hutchison HK IMS subtree has no new parameters
relative to the earlier audit; the two removed APN `default_ip_stack` keys do
not enter Switch's VoWiFi runtime. The local iOS project's 3HK selector and
Hutchison profile match Switch byte for byte, and the 3HK user template's
protocol fields match (Switch adds only local metadata). The focused Rust
runtime-profile test passed. SIM World remains the 3HK subscription selected
by home PLMN 454/03; this check did not modify carrier files, network policy,
or generate a DMG. A full upstream corpus refresh and independent oracle
rebaseline remain separate work.
