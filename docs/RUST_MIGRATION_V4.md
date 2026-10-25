> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# Rust migration V4 execution record

Status: selective-migration execution record in progress; it is **not** a
completion score or production ownership percentage. The authoritative
ownership ledger is currently 9/15 components (60%) in `ownership.rs`: Rust
owns the public gateway, reader identity/AKA, protocol cores, and local
archives, while live hardware/session bridges remain explicitly gated behind
Go until differential and real-device cutover evidence is complete.

## Product boundary

- SwiftUI/AppKit and the macOS CoreAudio layer remain native Swift.
- A single Rust daemon will replace the Go backend incrementally.
- The Go daemon remains the default and sole hardware owner until each Rust
  migration gate passes.
- Rust and Go must never open the same reader or DJI device concurrently.

## Foundation gate

The first Rust release owns no hardware. It must provide a Go-compatible Unix
socket launch argument, local launch-token authentication, a health endpoint,
structured runtime metadata, clean signal shutdown, and safe socket cleanup.

Passed in 0.19. Rust tests, strict Clippy, Go tests, Xcode Release build,
embedded-binary authentication, architecture, deployment target, signature,
and socket cleanup were verified.

## Gateway gate

0.20 adds a hidden, non-default supervised gateway. Rust owns the public Unix
socket, validates the application token, starts the bundled Go daemon on a
private child socket, and forwards the existing API without changing payloads.
Only Go opens hardware. Rust sends SIGTERM and allows bounded graceful cleanup
before using a forced stop.

The gateway binds its public socket immediately, so slow modem discovery no
longer looks like a Rust launch timeout. While Go is still starting, forwarded
requests return a structured retryable `go_backend_unavailable` response.

Both implementations and the Swift launcher now remove only stale Unix socket
files. Regular files and symbolic links at either listen path are rejected and
preserved.

## Typed read-shadow gate

0.21 adds `GET /api/v2/device-snapshot` without changing any existing UI API.
Rust concurrently reads and decodes Go health, module status, and reader
inventory, then conditionally reads the selected reader identity. The response
has an explicit schema version and independent issue records, so an absent DJI
module does not hide a healthy reader and a reader failure does not erase the
module snapshot.

The decoder accepts all currently valid Go response shapes, including null
signal values, absent optional SIM identity fields, no selected reader, and no
USB device. It also reports stale selections and selected/info name mismatches
instead of silently combining data from different devices.

## Swift shadow-consumption gate

0.22 lets the Swift dashboard consume `GET /api/v2/device-snapshot` only when
the hidden Rust gateway is explicitly enabled. Module health, module status,
reader inventory, selected reader, and reader identity are applied from one
schema-versioned response. Device writes, reader connection, eSIM, SMS,
VoWiFi, and call operations still pass through Rust to the sole Go hardware
owner.

The Swift consumer rejects unknown snapshot schema versions, records partial
issues, avoids applying reader state across an in-flight profile generation,
and falls back to the existing compatible endpoints if the aggregate request
fails. Traffic sampling remains independent, so a traffic-counter failure no
longer marks otherwise fresh device identity as stale. The default runtime is
still the direct Go daemon.

## Differential replay and failure-injection gate

0.23 records sanitized, synthetic Go-compatible fixtures for connected module
health, full radio/IMS status, selected reader inventory, and reader identity.
Rust decodes and reserializes these fixtures while tolerating additive fields,
which prevents a typed migration from silently dropping data consumed by the
Swift dashboard.

The Unix upstream client is exercised over real local sockets with bounded
test policies. The gate verifies the HTTP/1.1 Host header and per-launch token,
response timeout classification, response-size rejection before JSON decode,
malformed JSON classification, and non-success HTTP status preservation. The
production timeout and response cap remain unchanged. Runtime metadata reports
`replay-gate`, snapshot schema version 1, `default_runtime=go`, and
`cutover_ready=false`; passing this gate does not itself authorize a default
runtime switch.

## Bounded backend-supervision gate

0.24 keeps the Rust public Unix socket alive when the sole Go hardware owner
exits unexpectedly. Rapid failures are retried after 250 milliseconds, one
second, and three seconds; a fourth failure exhausts the budget and terminates
the gateway instead of entering an unbounded restart storm. Thirty seconds of
healthy child uptime resets the failure budget.

During backoff, forwarded requests remain bounded and return the existing
retryable `go_backend_unavailable` response. `GET /api/v2/runtime` exposes
`go_backend_restarting` and `go_restart_attempts` for precise diagnostics.
Runtime metadata reports `supervisor-gate`, while `default_runtime=go`,
`hardware_owner=go`, and `cutover_ready=false` preserve the migration boundary.

Acceptance used the signed macOS 26 arm64 Release bundle and an attached ESTKme
reader. Terminating only the gateway's Go child kept the same public socket
responsive, exposed the restart transition, launched a new child PID, and
restored reader enumeration. The isolated test process and both socket files
were then shut down cleanly.

## Read-recovery admission gate

0.25 adds an explicit `starting`, `ready`, and `restarting` availability state
for the Go child. Read-only GET requests arriving before the private child
socket is ready wait for at most five seconds and then continue on the same
public request. This removes avoidable transient failures during initial launch
and bounded child restart without hiding a persistent outage.

Mutating POST, PATCH, PUT, and DELETE requests are never replayed by Rust. They
fail immediately with `go_backend_restarting_write_not_replayed` while the
child is unavailable, preventing duplicate SMS submission, calls, profile
switches, or VoWiFi state changes. Runtime diagnostics expose the availability
phase and a monotonic child generation in addition to the restart count. The
stage remains hidden and non-default, with Go retaining sole hardware ownership.

Acceptance used the signed macOS 26 arm64 Release bundle and the enumerated
ESTKme reader. After the private Go socket disappeared, the earliest concurrent
GET remained pending and returned the restored reader list from generation 2;
a simultaneous POST was rejected before reaching Go. This also covered the
sub-monitor interval between socket loss and child-exit observation.

## Mandatory proxy compatibility

All future Rust VoWiFi work must implement the separately recorded 0.15 proxy
contract in `docs/VOWIFI_PROXY_V015_CONTRACT.md`. Later fake-IP/utun routing
experiments are not the migration baseline.

## Device workspace and VoWiFi proxy foundation gates

0.26 separated the module and reader workspaces in Swift without duplicating
device ownership or reader polling. Both pages still consume one dashboard
state source, while the reader page keeps the existing VoWiFi session and eSIM
generation guards.

0.27 makes reader VoWiFi shutdown independent from card presence and holds a
retiring lease until IKE/IMS goroutines release their resources. IMS REGISTER,
SMS readiness, and call readiness are now separate states, and Swift continues
health polling after registration instead of freezing the first successful
snapshot.

The new `proletarius-vowifi` crate owns the v0.15-compatible proxy policy,
strict SOCKS5 handshake/UDP codecs, and an asynchronous UDP ASSOCIATE relay.
It always sends ePDG as ATYP=DOMAIN, supports IPv4/IPv6 relay endpoints, rejects
fragmentation and malformed frames, and has no direct-public-UDP fallback.
Transparent fake-IP and verified utun evidence remain preferred when present;
otherwise a successful SOCKS5 UDP association is required. Rust still does not
open the PC/SC reader at this gate.

Surge 6.9 acceptance on 2026-09-01 confirmed the distinction in the live
environment: `127.0.0.1:6153` completed SOCKS5 authentication but rejected UDP
ASSOCIATE with REP 7, while all four public giffgaff ePDG answers routed through
`utun8`. The selected Surge path is therefore verified-utun, not SOCKS5 UDP and
never the physical interface. No PC/SC reader was enumerated during this pass,
so reader/IKE live acceptance remains a later gate rather than being inferred
from proxy success.

## Typed reader VoWiFi snapshot gate

0.28 adds the Go `/api/reader/vowifi` response to the Rust device snapshot as a
typed, additive sub-snapshot. Unknown upstream fields remain preserved for
forward compatibility. Rust captures reader identity and VoWiFi state
concurrently for the selected reader, while keeping their errors independent:
a VoWiFi status failure cannot erase healthy card identity, and an identity
failure cannot be presented as a successful reader refresh.

Swift now applies reader identity and VoWiFi state from the same snapshot
generation. In Rust gateway mode it no longer issues a redundant periodic
forwarded VoWiFi GET outside an active mutation, which removes cross-generation
UI combinations while retaining the existing Go fallback path.

Live read-only acceptance used the connected ESTKme reader and active giffgaff
profile. The schema-v1 snapshot returned matching reader identity, carrier
profile/ePDG, tunnel state, and IMS state; disconnect and gateway shutdown were
clean. No profile switch, IKE start, IMS mutation, SMS, or call was performed at
this gate.

## Rust PC/SC discovery gate

0.29 introduces the `proletarius-reader` crate. It establishes a macOS PC/SC
context and observes reader inventory, card presence, busy/mute flags, and ATR
length without connecting to a card, resetting it, or sending APDUs. A small
reconciliation state machine increments its revision only when the observed
inventory, card state, or valid selection changes; a removed reader can never
retain a stale selection.

The typed device snapshot now uses Rust PC/SC as its inventory source and keeps
Go as a fallback if discovery fails. Rust and Go inventories are compared as
sets during the migration gate. Go remains the only component allowed to open
the reader for eSIM, SIM identity, AKA, IMS, SMS, or calls.

Read-only live acceptance on 2026-09-01 found one ESTKme reader with a present,
unmuted, non-busy card and a 22-byte ATR. Rust and Go inventories matched with
no snapshot issue, and a second unchanged capture retained revision 1. The
isolated gateway and Go child then terminated cleanly; no card/profile or
VoWiFi mutation was performed.

## Rust SIM/AKA protocol gate

0.30 introduces the `proletarius-sim` crate. It builds SELECT-by-complete-AID
and 3G AUTHENTICATE APDUs, follows `61xx` GET RESPONSE chains with fixed count
and size limits, checks terminal status words, and strictly parses AKA success,
14-byte AUTS synchronization failure, and authentication rejection. Malformed
lengths and trailing secret bytes are rejected without logging the challenge or
response payload.

The Rust encoding matches the existing uicc-go command contract: 16-byte RAND,
16-byte AUTN, P2 `0x81`, and length-prefixed RES/CK/IK. Unit tests cover partial
challenges, prefix-only AIDs, truncated fields, incorrect CK/IK lengths,
non-success status words, unbounded GET RESPONSE attempts, and trailing data.
The existing Go reader and IMS SIM-auth suites also pass as the differential
baseline. No live AKA challenge was issued at this gate, and Go remains the
sole owner of real card sessions.

## Rust IKEv2 wire and message-window gate

0.31 introduces the `proletarius-ike` crate. It strictly encodes and decodes
the 28-byte IKEv2 header, validates declared packet length and major version,
and separates UDP/4500 IKE packets from ESP using the four-byte non-ESP marker.
Packets are capped at 1 MiB before allocation or payload processing.

The session tracker enforces non-zero and stable initiator/responder SPIs, one
outstanding request, exact exchange/message-ID response matching, bounded
duplicate handling, and legal INIT/AUTH/ESTABLISHED/DELETE transitions.
Multiple IKE_AUTH EAP-AKA rounds remain in `Authenticating`; only an explicit
final AUTH/configuration/Child-SA/traffic-selector verification call can expose
the session as established.

Rust unit tests cover wire round-trip, IKEv1 rejection, truncation, forged
lengths, ESP/IKE NAT-T separation, SPI changes, out-of-window replies,
duplicates, multi-round authentication, and orderly delete. Existing Go IKEv2,
IPsec, and SWu suites pass as the differential baseline. This gate sends no
network packet and does not take ownership from the live Go session.

Live acceptance additionally found a Go compatibility-path defect: when host
DNS returned public ePDG addresses, a local Clash SOCKS5 endpoint with working
UDP ASSOCIATE was being bypassed in favour of a generic utun route. The fixed
selection keeps fake-IP transparent routing first, then delegates the original
ePDG hostname to the explicitly configured SOCKS5 UDP endpoint; verified-utun
is only the fail-closed fallback when SOCKS5 UDP is unavailable. No public UDP
direct path was introduced. With `127.0.0.1:7897`, giffgaff completed IKEv2,
EAP-AKA, IPv6 Child SA, IMS AKA/IPsec registration, SMS readiness, and call
readiness twice, including stop and immediate restart without reader replug.
An independent 75-second startup deadline now releases a silent ePDG session.

## Rust IMS/SIP protocol gate

0.32 introduces the `proletarius-ims` crate as a non-networking shadow core.
It strictly frames SIP responses, caps message/header sizes, rejects folded or
malformed headers, duplicate Content-Length, body-length mismatch, and invalid
REGISTER transaction metadata. The state tracker binds responses to the exact
Call-ID and CSeq, bounds repeated AKA challenges, distinguishes provisional and
final responses, and enforces disabled/auto/required sec-agree policy. The
state tracker does not retain or log AKA challenge material.

The first differential fixture models the sanitized live giffgaff sequence:
initial REGISTER, 401 AKAv1-MD5 plus Security-Server, authenticated REGISTER,
and 200 registration. Go remains the only owner of the live IMS socket, IPsec
SA, SIM AKA operation, SMS, and call runtime at this gate.

Acceptance passed nine Rust IMS unit tests, the full 57-test Rust workspace,
strict Clippy, the existing Go IMS/header/SIM-auth/reader suites, and a macOS
26 arm64 App build. The embedded 0.32 gateway exposed the new contract and
matched Rust PC/SC inventory against the attached ESTKme reader without opening
the card or mutating the already live-tested Go VoWiFi path.

## Rust IMS SMS/RPDU protocol gate

0.33 introduces the `proletarius-sms` non-networking shadow core. It strictly
parses mobile-terminated RP-DATA, validates semi-octet addresses and exact TPDU
lengths, builds mobile-originated RP-DATA without circuit-switched CP framing,
and encodes RP-ACK/RP-ERROR. Delivery reports accept both direction variants,
mask the cause extension bit, reject trailing/truncated IEs, and correlate each
RP message reference exactly once through a bounded pending set.

The initial differential fixtures match the existing Go giffgaff-compatible
RP-MT-DATA and SMSC encoding. The live Go IMS service remains the sole sender,
receiver, persistence writer, and owner of SIP MESSAGE transactions.

Acceptance passed five Rust SMS tests, the full 62-test Rust workspace, strict
Clippy, and the existing Go RPDU, SMS delivery, IMS, TPDU codec, device, and
reader API suites. No external SMS was sent at this shadow-only gate.

## Rust IMS call/RTP protocol gate

0.34 introduces the `proletarius-call` non-networking shadow core. Its outgoing
INVITE state machine distinguishes provisional, reliable early-media, final,
local-cancel, network-reject, and CANCEL/200 race paths. A locally requested
CANCEL followed by 487 is a clean end, while a network-originated 487 remains a
failure; all non-2xx finals still require ACK, and retransmitted finals return
ACK-only actions so media or BYE cannot be duplicated.

The RTP parser validates version, CSRC, extension and padding bounds before
exposing payload. Sequence tracking drops duplicate/late packets, bounds loss
concealment to three frames, handles 16-bit wraparound, and resets cleanly on
SSRC handover. RFC4733 event mapping covers `0-9`, `*`, and `#` with explicit
duration/end-bit payloads. CoreAudio and real IMS media sockets remain owned by
Swift/Go at this gate.

Acceptance passed seven Rust call tests, the full 69-test Rust workspace,
strict Clippy, the existing Go IMS call/voice/runtime differential suites, and
a macOS 26 arm64 App build. Live CTExcel acceptance used the manually entered
Clash proxy `127.0.0.1:7897`: IKEv2/EAP-AKA, the IPv6 Child SA, IMS AKA with
3GPP IPsec, SMS and call readiness all completed. An actual `BAL` SMS to `888`
was accepted and the operator reply was decoded and persisted. An actual call
to `888` progressed from `100 Trying` to `200 OK`, negotiated AMR plus RFC4733,
exposed media readiness, and ended on the network BYE without a crash or a
false SIP 487 failure. The measured call record was 18 seconds.

The same live session then stopped without removing the reader and immediately
re-established IKE, IMS, SMS, and call readiness. A normal SIP response split
across TCP reads is now retained by `ParserStream` without emitting a false
`SIP partial data` warning; malformed complete frames remain reported.

## Component ownership manifest gate

0.35 makes the remaining migration boundary machine-readable instead of
relying on comments or a single optimistic boolean. The native Rust endpoint
`GET /api/v2/migration-readiness` reports a versioned manifest for the gateway,
aggregate snapshot, DJI module, reader session, reader VoWiFi runtime,
message/call archive, and macOS audio bridge. Each component has exactly one
declared Rust, Go, or Swift owner and an explicit Go-removal blocker.

The runtime `cutover_ready` field is derived from this manifest. It therefore
cannot become true merely because protocol shadow tests passed. Reader APDU and
AKA session ownership, the live IKE/ESP/IMS runtime, archives/device routing,
and DJI USB/QMI remain blocking Go-owned components until their separate
runtime acceptance gates complete. Native v2 diagnostic routes are classified
as Rust-owned; legacy and mutating routes remain explicitly classified as Go
compatibility paths at this stage.

Acceptance passed the full 72-test Rust workspace, strict Clippy, and a macOS
26 arm64 App build. The isolated embedded 0.35 gateway returned matching
runtime and ownership schema versions, kept `cutover_ready=false`, enumerated
the attached ESTKme reader through Rust PC/SC, and preserved the legacy reader
API through the sole Go card owner. A controlled Go-child termination kept the
Rust gateway alive, advanced the compatibility generation, and restored the
child under the bounded supervisor. The isolated processes and sockets then
shut down cleanly without changing the active eSIM profile or VoWiFi state.

## Rust call archive ownership

The first 0.36 runtime cutover moves `GET /api/calls`,
`POST /api/calls/clear`, and `POST /api/calls/delete` to Rust. The
`proletarius-store` core preserves unknown JSON fields, rejects malformed,
oversized, non-object, symbolic-link, and non-regular inputs, caps archives at
1,000 newest records, and replaces files atomically after data and directory
synchronization. A shared advisory lock serializes Rust mutations with call
records still produced by the Go device runtime.

The Go producer now reloads the archive under that cross-process lock before
every add/remove/clear, writes through a synchronized temporary file, and
refuses to overwrite corrupt or symbolic-link targets. Differential fixtures
cover current reader and module call records, RFC3339 timestamps, and additive
future fields. Concurrent independent writers were repeated without a lost
record, while a corrupt fixture remained byte-for-byte intact after mutation.

Isolated API acceptance used a temporary HOME, not the user's real archive.
Rust listed both Go-format records, deleted one exact ID, preserved an unknown
field on the other, and cleared the archive with unchanged Swift response
shapes. With the Go child suspended, both the call-history and ownership
endpoints remained responsive, proving that call archive reads and mutations
no longer depend on Go. SMS persistence and live call production remain
separate blocking components.

After the gateway, recovery, reader discovery, protocol, ownership-manifest,
and first native API gates passed, Swift now launches the Rust gateway by
default. The Go process remains a supervised child and the sole owner of every
hardware component still marked as blocking. A hidden
`ProletariusBackendMode=go-compatibility` preference provides an emergency
diagnostic fallback without exposing another user-facing mode or allowing two
hardware owners to run concurrently.

## Rust local SMS archive ownership

The 0.36 cutover adds a Rust-native owner API for the local SMS archive without
changing the proven live IMS/module producer or the stable aggregate SMS API.
Rust now natively serves
`GET /api/v2/sms-archive` plus clear, exact-ID delete, and exact-sender delete
mutations. These routes remain responsive when the supervised Go child is
suspended. The ownership manifest separates `message_archive` (Rust) from
`message_runtime` (Go), so the successful persistence cutover cannot be
misreported as permission to remove live SMS device routing.

The Go compatibility producer now takes the same advisory lock as Rust,
reloads before every mutation, rejects corrupt, oversized, symbolic-link, and
non-regular archives, and replaces data through a synchronized temporary file
and directory sync. Existing entries retain their original JSON shape; both
owners calculate a Go-compatible stable ID at the API boundary when an old
record has none, while newly produced records persist it immediately. Two
independent Go archive handles wrote 40 concurrent records without loss, and
corrupt input remained byte-for-byte intact.

Acceptance passed 80 Rust tests, strict Clippy, the full Go suite, and an arm64
macOS 26 Release build. An isolated temporary HOME fixture listed two records,
preserved an additive unknown field, deleted one exact ID, then deleted the
remaining sender. No real SMS archive was read or mutated. The test gateway
and child exited cleanly and removed both sockets.

## 0.80 reader AKA AID ownership

Rust now discovers the complete USIM or ISIM AID from EF-DIR and executes AKA
inside the same serialized PC/SC transaction. The Go VoWiFi caller no longer
opens the reader or enumerates AIDs before invoking the native AKA endpoint.
When the native socket is not configured, the existing Go card path remains
available. Missing ISIM preserves the explicit fallback condition rather than
silently selecting USIM; an AKA rejection or transport error never triggers a
second authentication attempt through Go.

Scripted APDU tests cover full-AID ISIM selection, the no-ISIM path without any
AUTHENTICATE command, and malformed request selectors. Go tests cover native
request encoding and missing-ISIM fallback. This protocol gate alone does not
prove live IMS registration: an operator-issued RAND/AUTN challenge must still
be exercised with the attached card before this gate is accepted as live.

## 0.81 live AKA/IMS acceptance and packet-size diagnostics

On macOS 27 with the attached ESTKme reader and the active giffgaff profile,
the isolated Rust gateway used the explicitly supplied Clash SOCKS5 endpoint
`127.0.0.1:7897`. The first attempt completed the Rust USIM AKA challenge and
IKE/ESP tunnel but timed out waiting for an initial IMS REGISTER response on
both network-provided P-CSCF candidates. A second attempt, with the same card,
proxy and code, completed Rust USIM AKA, IMS 3GPP IPsec registration, and
reported SMS/call readiness. No SMS or call was placed. This is evidence for
the live USIM AKA gate, not proof that the intermittent IMS response timeout
is solved.

The userspace SWu stack now records only maximum outbound inner-packet length
and the count above 1280 bytes when a REGISTER attempt fails. This bounds a
future MTU diagnosis without exposing payloads or changing the working MTU
before packet-size evidence exists. The test session was stopped and its
temporary logs and sockets were removed.

## 0.100 CTExcel reader VoWiFi live smoke test

On 2026-09-13, the attached ESTKme reader was connected through an isolated
0.100 Debug Rust gateway (the subsequent 0.101 change affects only fax SSH
multiplexing), and the active card resolved to the `apple_orange_uk` profile
(`Orange / CTExcel UK`). With the explicitly supplied Clash endpoint
`127.0.0.1:7897`, a live start progressed through EAP-AKA, connected IKE/ESP,
registered IMS, and reported both SMS and call readiness. The stop request
returned HTTP 200; the isolated gateway and its Go hardware child exited.
The same local proxy separately accepted SOCKS5 UDP ASSOCIATE. No message or
call was sent in this smoke test, and no conclusion about sustained media or
carrier-delivered SMS follows from the readiness flags alone.

The first test harness attempt did not connect the PC/SC reader before asking
for VoWiFi status; a second attempt used an overly narrow `ctexcel` substring
guard even though the correct profile ID is `apple_orange_uk`. Neither attempt
reached the network. The successful third attempt explicitly connected the
reader and accepted the verified CTExcel UK profile.

## 0.102 CTExcel SMS and 888 call live exercise

Using the current 0.102 Debug app backends and the same attached CTExcel card
and manually supplied Clash endpoint, the isolated gateway reached registered
IMS with `sms_ready=true` and `call_ready=true`. One `BAL` SMS sent to 888 via
the explicitly selected reader device returned HTTP 200. A subsequent
read-only inspection of the persisted local archive found exactly one new
outbound BAL record and one nonempty inbound 888 reply on the reader, about
one second apart. The temporary test harness's metadata poll reported no
reply, so that poll is **not** accepted as evidence; the persisted archive
provides the stronger receive evidence. No SMS was sent more than once.

One 888 call returned HTTP 202, transitioned to `active`, and reported
`media_ready=true`. Its negotiated codec was AMR-NB, not PCMA. Local SDP offers
PCMA first and AMR-NB mode 7 second; the carrier's choice of AMR is consistent
with previous PCMA-only 888 attempts ending in SIP 487 / Q.850 cause 127.
Do not remove the AMR fallback just to make the displayed codec read PCMA:
that would regress the now-working service-number call. A headless gateway
cannot prove Mac microphone/speaker or AirPods two-way quality; an in-app
audio test remains required. Hangup and VoWiFi stop both returned HTTP 200,
and the isolated processes exited without deleting real messages.

## Cutover rule

A component may become the default Rust implementation only after recorded
fixture replay, automated tests, failure injection, and real-device acceptance
all pass. Removing the corresponding Go implementation is a separate final
gate.
