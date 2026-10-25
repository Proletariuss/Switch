# Iteration 550 — historical SIM World 2.86 comparison

Follow-up: [Iteration 552](iteration-552-simworld-286-rust-registration-ab.md)
records a subsequently authorized, guarded registration comparison. The old
client registered; current Rust also failed when pinned to the successful old
ePDG address through the same observed proxy endpoint/policy. The feasibility
section below describes the earlier static assessment, before those preflights
were resolved.

2026-10-18. SIM World is the 3HK subscription selected by home PLMN 454/03.
The [2.86 carrier record](../2026-10-07-evs-wideband.md) documents successful
registration and a user-confirmed clear AMR-WB call. It also names
`build/evs/Switch-EVS-only.app`: that test registered SIM World before its
EVS-only call received SIP 488. Consequently, the current failure at IMS TCP
`plain_connect`, with ESP 8 submitted / 0 received, is a loss of previously
observed functionality. The historical observations do not establish that
today's network route, proxy exit or operator state is identical.

## Archived binary and embedded template

The retained app is under
`/Users/StatTrak/Desktop/Switch_MAC/_legacy/Switch_MAC-2.96-20261013/build/evs/Switch-EVS-only.app`.
Its plist reports version **2.86**. Its `Contents/Resources/backend/switchd`
SHA-256, read in this audit, is
`d488e985c0ffbbf512196fe438f7dc14eacb28ff834a267d69be26326e7bb6df`.
`go version -m` identifies SWu-Go, gVisor and sipgo dependencies, with
`vcs.revision=8aa7b517afd2bc4ef8c10373638721be2c90a84e` and
`vcs.modified=true`. This is an actual Go VoWiFi artifact; the later 3.42
Go child is not a substitute for this historical comparison.

Read-only extraction searched the binary bytes for `"id": "3_hk"`, found the
preceding JSON opening brace and decoded that object with Python's
`json.JSONDecoder.raw_decode`. A recursive field comparison against current
`data/carriers/user_templates/3_hk.json` found only two additions:

| Current field | Archived 2.86 object | Current object |
| --- | --- | --- |
| `ims.include_pani` | absent | `false` |
| `ims.include_pani_authenticated` | absent | `false` |

The complete IKE and EAP objects, and all other IMS fields, match. This includes
`wlan.three.com.hk`, IPv4, local UDP 500, ESP `aes256-sha256`, the ordered IKE
offers, ISIM identity, EAP-direct authorization and sec-agree-off transport.
The retained Go source maps absent PANI booleans to false. Moreover, PANI and
other REGISTER headers cannot explain failure before TCP connects.

## Post-CHILD path comparison

Seven relevant files in the retained snapshot's `src/backend` are byte-identical
to their retired Go `HEAD:backend/...` versions: `reader_vowifi_transport.go`
and `reader_vowifi_effective_config.go` in `cmd/proletarius-macos`;
`internal/vowifi/voiceclient/swu_netstack.go`; and SWu-Go's `pkg/ipsec/esp.go`,
`pkg/ipsec/socket.go`, `pkg/swu/state_auth.go` and
`pkg/swu/dataplane_netstack.go`. The latter four paths are under
`backend/third_party/swu-go`.

Comparison with current Rust found no supported narrow production fix:

- Carrier precedence remains request ePDG/port, then matching YAML override,
  then the scoped 3HK template. The template applies after carrier selection,
  preserving more specific MVNO matches. See
  `carrier_session_config.rs:32` and `carrier_runtime_profile.rs:307`.
- The old reader transport pins the already selected numeric ePDG address and
  leaves interface selection to the OS. Rust `transport.rs:116` also binds an
  unspecified local address, preserves the requested local port, and uses
  ordinary OS routing. Both retain the channel while moving the destination
  from 500 to 4500. ESP carries no Non-ESP Marker. The actual receive-socket
  difference was already tested without improvement in
  [Iteration 544](iteration-544-simworld-udp-port-ab.md).
- Initial CHILD KEYMAT order, outbound remote SPI, CBC IV/padding and truncated
  SHA256 ICV agree. Both inner stacks use MTU 1280. The recent offline CBC
  final-EAP/CHILD/SYN composition test independently checks these boundaries;
  it does not inspect historical or current real-session packets.
- Go's `selectIMSPathForConfig` and Rust's
  `native_vowifi_session/registration_candidates.rs:17` prefer a numeric
  override, otherwise use CFG_REPLY P-CSCF and a same-family tunnel address.
  Both prefer IPv6 for dual stack; this template explicitly requests IPv4.
  `auto` selects TCP, normally using local and remote port 5060. Rust's
  `registration_coordinator.rs:1589` waits for TCP establishment before sending
  REGISTER, so this observed failure precedes IMS header/security negotiation.
- Rust counts inbound ESP before decrypting it (`child_data.rs:341`). Zero ESP,
  zero rejected ESP and zero post-CHILD IKE/keepalive/invalid datagrams do not
  support diagnosing an inbound checksum or selector rejection inside smoltcp.

## Verification boundary and next evidence

The archived binary was not started. No hardware, card state, installed app,
proxy/network configuration, SMS, call or DMG action occurred. No production
code or carrier data changed. The current focused 3HK test filter passed
**8 tests, 0 failures** with `cargo test --locked -p proletarius-vowifi three_hk`.

The dirty 2.86 build cannot be reconstructed exactly from its revision string;
the retained 2.96 source snapshot is not proof of every source byte used to
build 2.86. The historical document names the matching app path but supplies
no contemporaneous binary hash. This audit verifies the archived artifact's
present metadata/template, and source equivalence where explicitly stated.
It performs **no old/new route or real ESP-packet comparison** and proves no
cause for today's zero return traffic.

The next discriminating evidence is a bounded same-card, same-network
registration comparison with the archived 2.86 client and current Rust,
retaining only route/endpoint equality and packet-class counts, or an exit-side
UDP 4500 counter. Neither was executed here. Until that evidence exists,
changing the 3HK template or claiming a proven ESP regression is unsupported.

## Registration A/B feasibility: not executed

The existing probe sends `{"enable":true,"proxy":""}`
(`scripts/probe-reader-vowifi.py:170`). A nonempty `proxy:"direct"` is not a
supported bypass switch: the retained Go parser requires `host:port`
(`reader_vowifi_session.go:1028`) and rejects it before startup. A real explicit
loopback SOCKS endpoint suppresses Go's automatic Surge recovery, but does not
preserve the present automatic transport-selection contract. Current Rust marks
that input `explicit`, gives it no Surge DNS ownership, and refuses resolver
fallback if its SOCKS UDP ASSOCIATE is unavailable
(`transport_selection.rs:331,423`). The archived gateway instead permits host
resolution/direct fallback. Substituting a dummy endpoint or relying on that
old fallback would not be a controlled comparison of the existing proxy path.

The retained Go recovery uses the absolute
`/Applications/Surge.app/Contents/Applications/surge-cli`, with read arguments
`feature get system-proxy` / `feature get enhanced-mode`, and write arguments
`feature set enhanced-mode on` (`reader_vowifi_session.go:806,957,1001`).
A PATH shim cannot intercept either operation. Thus this audit has not
established an argument-selective write guard that retains automatic proxy/DNS
selection and the exact archived binary. No old registration attempt was run.

Before a later controlled test, startup also needs exclusive reader ownership,
an empty isolated data root, and no DJI modem: the retained launcher can
automatically configure a discovered module's flight mode. Persisted forwarding
and N-wait recovery must not be imported. These prerequisites and the network
write guard need resolution before enabling the old session.
