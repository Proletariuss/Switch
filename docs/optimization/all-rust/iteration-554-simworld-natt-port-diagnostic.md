# Iteration 554 — SIM World NAT-T port diagnostic

2026-10-18. The archived 2.86 Go client registered the same SIM World 454/03
card, while the prior Rust baseline pinned to that successful ePDG failed after
CHILD establishment with ESP 8 sent / 0 received. A separate current Rust
Release build with the optional IKE DEVICE_IDENTITY(41101) also failed 8/0.

To test the remaining UDP port-following hypothesis without changing the
production source, an isolated **pre-41101** Rust copy was built under
`build/diagnostics/natt-port/`. It retains the selected ePDG and Surge path,
receives candidate UDP datagrams from the same ePDG IP with their source ports,
but keeps the send destination fixed. A port is recorded only after the final
responder AUTH, CHILD proposal, traffic selectors and configuration have been
validated. The first successful ESP send records its destination port. Invalid
IKE cannot update the accepted observation or send target. The patch and
offline validation are described in the diagnostic copy's `README.md`.

The one registration-only test reused the previously verified numeric ePDG,
automatic Surge Enhanced Mode UTUN route and the ESTK reader's SIM World 454/03
profile. It switched/downloaded no profile, sent no SMS and placed no call.
The diagnostic binary SHA-256 was
`91c0fb75181e52a151a7446c5d135d7b4c47e7897dd82ffb41d3e779ef322907`.
Report: `build/reports/simworld-554-natt-port-diagnostic.json`.

| Observed condition | Result |
| --- | --- |
| Final verified IKE_AUTH reply source port | 4500 |
| First outbound ESP destination port | 4500 |
| ESP sent / received / rejected | 8 / 0 / 0 |
| Post-CHILD IKE / keepalive / invalid datagrams | 0 / 0 / 0 |
| IMS registered | No |
| Same pinned ePDG selection | Yes |
| Surge flags unchanged | Yes |
| Disable, reader disconnect, daemon exit, private runtime removal | All confirmed |

The verified response port and ESP target were equal. Thus lack of Go-style
port following **did not explain this observed failure**. The earlier
receive-only relaxation also failed; neither result proves that all proxy
paths or later NAT rebinding behave identically. Because the diagnostic copy
predates 41101, its comparison is with the previously recorded pre-41101
pinned Rust baseline, not a single-variable comparison with the new Release.
The isolated receive change is not a production fix and was not merged.

The remaining high-value evidence is the actual accepted CHILD transform and
SPI roles, CFG_REPLY local/P-CSCF choices, TSi/TSr membership and independent
integrity/decryption/checksum booleans for the first ESP packet. These can be
observed without recording identities, IPs, SPI values, keys or packet bodies;
no such live probe has yet been run. SIM World registration, SMS and calls on
the current Rust backend remain unverified or failed, so P6/full migration
acceptance cannot be marked complete and no DMG was generated.
