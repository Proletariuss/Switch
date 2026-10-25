# Iteration 552 — SIM World 2.86 / Rust registration A/B

2026-10-18. A bounded, registration-only comparison confirmed that the archived
**2.86 Go client still registers SIM World 454/03**, while the tested Rust
backend times out connecting IMS TCP. A further Rust attempt pinned to the
successful old client's actual ePDG address reproduced ESP **8 submitted / 0
received / 0 rejected**. This establishes a current old/new behavioral
difference; the cause is not yet identified.

## Preconditions and artifacts

The archived app is the EVS-only 2.86 bundle identified in
[Iteration 550](iteration-550-simworld-286-comparison.md). Its Go backend SHA-256
is `d488e985c0ffbbf512196fe438f7dc14eacb28ff834a267d69be26326e7bb6df`;
its paired gateway SHA-256 is
`882ab9fba2df80a99b2e8ee9780dad668437bcbbfeae05712eab1b3214c00f00`.
The tested Release Rust daemon,
`build/rust-target/release/switchd-rs`, has SHA-256
`ed79ac298712a1d59629dc1a76143787d287b34fa70e9b1914bb1dc5a4f59be6`.
These hashes identify the binaries tested, not later source edits or rebuilds.

Read-only USB inventory found no DJI/Quectel device. No UDP 500/4500 owner was
present before testing. The installed backend's status request timed out, so
testing initially stopped. A subsequent full descriptor inspection found only
its listeners/self-pipe/KQUEUE, no device/PCSC/UDP socket, and no active GUI.
With explicit coordination, that dormant instance was left untouched and the
isolated reader connection/454/03 identity read was accepted as the remaining
ownership preflight.

Each arm used a fresh private data directory, short private socket and new
token. The first old/current pair shared one newly generated valid test IMEI;
their transient card-identity comparison was equal. The later pinned Rust
attempt used another fresh directory/session. No existing application data was
copied. The guard required automatic `enhanced-mode-utun`, an actual utun
interface, `surge-effective-profile`, a unique ESTKme-RED and SIM World 454/03
before enable. No manual DNS substitution or direct ePDG mode was authorized.
The old gateway retains its historical system-resolver behavior; this audit
does not claim that its DNS mechanism is identical to current Rust's
Surge-owned DNS selection. Both observed UDP paths were captured by Surge.

## Results and endpoint control

| Attempt | Observed result | Outer ESP diagnostic |
| --- | --- | --- |
| Archived 2.86, automatic proxy | IKE/CHILD established; IMS registered; readiness flags true | Not exposed by old status; unavailable |
| Rust, automatic proxy | IKE/CHILD established; IMS `plain_connect` timed out | 8 submitted, 0 received, 0 rejected; all post-CHILD receive classes 0 |
| Rust, automatic proxy plus existing ePDG override | Same failure after pinning `124.217.186.49` | Same 8/0/0 and zero post-CHILD receive classes |

The first pair's transport **preflight** addresses differed. Startup selects its
transport again, so those values alone did not identify the actual handshake
peers. The old successful process was then located by exact archived process
path and PID in Surge's read-only recent request history. Its carrier target
was `remoteHost=124.217.186.49:500`. The separate `remoteAddress` was the proxy
endpoint and was not used as the ePDG override.

Surge's own numeric DNS lookup returned `124.217.186.49` unchanged. The pinned
Rust preflight selected exactly that address with automatic Surge ownership
and a verified utun route. A live and subsequently completed request record
confirmed the actual target. Final exact-process metadata comparison:

| Surge UDP request | Outbound bytes | Inbound bytes | Policy |
| --- | ---: | ---: | --- |
| Successful archived Go PID 84064 | 6492 | 3040 | Hong Kong 14 residential |
| Pinned Rust PID 84511 | 2051 | 1188 | Hong Kong 14 residential |

The carrier target, policy name and proxy endpoint compare equal; both records
report `en0`, completed and `failed=false`. Surge retains the original
`:500 (Port Map)` flow label. These are total UDP-flow bytes, including IKE,
not ESP packet counts or proof of exit/ePDG delivery. The Rust receive-class
counters establish that its observed inbound total did not include any
post-CHILD datagram delivered to the application. `failed=false` is not an IMS
success result.

## Cleanup, evidence and remaining boundary

There was one enable request per attempt. Each was explicitly disabled, its
reader disconnected, its daemon exited with code 0, its owned process group
disappeared and its runtime directory was removed. Recorded test PIDs are gone;
installed PID 4114 remains untouched. Surge system-proxy and enhanced-mode
stayed On after every attempt; no restoration or policy change was needed.
No profile switching/download, SMS sending, dialing or DMG action occurred.
Readiness flags in the successful old status do not constitute a new SMS/call
business test.

Reports and the prepared harness reside under the Switch_MAC workspace build
directory, outside `src/build`:

- `build/reports/simworld-286-current-registration-ab.json`
- `build/reports/simworld-286-successful-epdg-evidence.json`
- `build/reports/simworld-current-pinned-old-epdg.json`
- `build/reports/simworld-current-pinned-proxy-evidence.json`
- `build/reports/simworld-286-current-proxy-final-counters.json`
- `build/tmp/simworld_registration_ab.py`

Only matched test-process metadata was retained, not the unfiltered Surge dump,
SIM identities, keys, SIP messages or packet payloads. P-CSCF/local-address
equality is unavailable because the terminal Rust error status cleared those
fields; it is not recorded as a proven endpoint difference. No real IKE/ESP
wire or negotiated-key comparison was performed. Matching the successful
ePDG/proxy path rules out address selection alone for this follow-up, but does
not yet locate a Rust defect or establish which inner target the ePDG accepted.

## Read-only NAT-T / CHILD comparison after the A/B

The pinned Rust request's Surge notes explicitly record new remote targets
`124.217.186.49:500`, then `124.217.186.49:4500`. Its later `ICMP closed`
entry coincides with the connection timeout/teardown boundary and is not
evidence of the initial cause. Original `remoteHost :500` labels therefore do
not mean that Rust failed to move NAT-T traffic to 4500.

One source-level difference remains testable: old SWu-Go's `pkg/ipsec/socket.go`
accepts `ReadFromUDP` from the selected ePDG IP, updates `RemoteAddr.Port` when
the source port differs, and uses that mutable destination for `SendESP`.
This port update precedes IKE/ESP authentication; it is selected-IP filtering,
not proof of an authenticated peer. Rust `transport.rs:193,214` reconnects its
socket to the chosen port and reads only that connected peer. Source-port
following must not be copied into production on the assumption that it caused
this failure. The receive-relaxation experiment in Iteration 544 did not cure
the earlier fault, and did not establish equivalence to destination-port
following on this successful old target. **The port-following hypothesis is
unconfirmed.** No new experiment or production socket change was made here.

For CHILD SA, the common supplied template offers only `aes256-sha256` ESP.
Iteration 523 observed Rust selecting AES-256-CBC/HMAC-SHA256-128; this A/B did
not retain either client's exact negotiated SA payload or keys. Source review
still agrees on initial `Ni || Nr` KEYMAT, outbound encryption/integrity before
inbound encryption/integrity, remote SPI for outbound ESP, sequence starting at
1, 16-byte CBC IV, numbered block padding, IPv4 next-header 4 and 16-byte ICV.
The independent offline first-SYN composition test passes, but does not prove
the actual packets/keys emitted in these real sessions match the peer's state.

The next useful instrument is bounded, payload-free metadata for the actual
500-to-4500 transition and first ESP: selected/source/destination UDP ports,
packet class and length, plus negotiated transform IDs/key lengths, SPI role
consistency and inner IPv4/TCP endpoint-policy/checksum booleans. Compare old
and Rust on the same pinned ePDG/policy, authenticating any port-update candidate
before treating it as peer evidence. Do not retain key bytes, raw IKE/SIP or
inner packet payloads. Another live session requires separate coordination;
none was started after the pinned follow-up.

## AUTH proposals, P-CSCF selection and the next single-connection diagnostic

The retained Go and current Rust builders both put IDi/IDr(APN), CFG_REQUEST,
CHILD SA and TSi/TSr in the initial AUTH. IPv4 CP requests are attributes
1/3/20 (address/DNS/P-CSCF), all with empty values; IPv4 selectors initially
cover all addresses, protocols and ports. Both effective configurations send
CP in first AUTH only when the supplied template has both CP flags true.
The same ordered IKE offers and sole `aes256-sha256` ESP offer were verified
in the actual archived binary's embedded template, as recorded in 550.

There is a concrete source-level AUTH difference beyond those equal fields:
old Go `pkg/swu/state_auth.go` appends MOBIKE_SUPPORTED (16396), TICKET_REQUEST
(16410), and an enabled/available IMEI DEVICE_IDENTITY (41101). Its effective
reader policy defaults device identity to enabled and its SIM adapter supplies
the generated IMEI. The archived 2.86 binary contains the device-identity encoder
and adapter function names. Rust `auth_config.rs:105` emits EAP_ONLY and optional
INITIAL_CONTACT, but deliberately omits unimplemented MOBIKE/resumption
capability declarations. Its `NativeTunnelSettings`/`NativeTunnelOptions` have
no IKE IMEI input and there is no current 41101 encoder in the IKE/VoWiFi crates.
The IMS SIP device identity is separate. **The missing IKE DEVICE_IDENTITY is
a compatibility gap; its causal link to this zero-ESP fault is unproven.**
This is not justification to advertise unsupported MOBIKE/resumption or to
change production behavior without the agreed implementation/validation step.

CFG_REPLY selection also needs an actual-session evidence boundary: Go
`selectIMSPathForConfig` and Rust
`native_vowifi_session/registration_candidates.rs:17` give numeric P-CSCF
override priority, otherwise choose advertised peers matching the requested
family, and retain the first matching assigned local address. Dual stack
prefers IPv6 in both; 3HK here requests IPv4. Current candidate expansion also
handles a self-address registrar by selecting another advertised transport
gateway. No observed old/current P-CSCF list, selected transport address or
assigned inner-address comparison survived this A/B's terminal Rust status.
Source equivalence does not fill that gap or prove the peer selected is usable.

The minimal next diagnostic is one current-Rust registration to the same pinned
ePDG and observed proxy policy, with production protocol decisions unchanged:

1. At initial/final AUTH, retain only notify-type presence, selected CHILD
   transform IDs/key lengths and SPI-role consistency booleans.
2. Before terminal cleanup, freeze CP address/P-CSCF counts and family, selected
   peer-is-local, source-in-TSi and target-in-TSr booleans. Do not log identities,
   private addresses, keys or packet bodies.
3. For only the first outbound ESP, independently verify its ICV/decryption,
   first sequence, selected remote SPI and the inner SYN checksum/endpoint
   relationships; record booleans plus outer source/destination ports and size.
   Preserve the existing received-class counters through timeout and cleanup.

This can separate a real-session key/frame/endpoint error from the missing
AUTH metadata and outer receive-tuple hypotheses without another broad retry.
Any receive-tuple observation requiring packet capture or changed socket
semantics must be separately coordinated; no such change or connection has
been performed in this read-only follow-up.
