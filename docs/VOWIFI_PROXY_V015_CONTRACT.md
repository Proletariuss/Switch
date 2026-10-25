# VoWiFi transport contract

This document keeps the proven 0.15 SOCKS5 behaviour while recording the
current automatic transport policy used by the Rust control plane.

## Baseline and target

- Baseline: Clash/mihomo at `127.0.0.1:7897` can establish ePDG, IKE/NAT-T,
  ESP, IMS and SMS by SOCKS5 UDP ASSOCIATE with the original ePDG domain.
- Target: the same flow must also work when Surge is selected as the macOS
  system proxy, without hard-coding a proxy into a carrier profile.
- Success measure: a real reader reaches `tunnel_state=connected`,
  `ims_state=registered`, and `sms_state=ready` after starting with an empty
  manual proxy field while Surge is the active system proxy.
- Failure budget: transport selection must fail with an actionable message
  instead of waiting for the 75-second IKE timeout when required takeover is
  unavailable.

## Current transport policy

1. A manually entered proxy is preferred. If it is blank, the application
   reads the active macOS SOCKS system proxy using System Configuration.
2. A working SOCKS5 UDP ASSOCIATE keeps the ePDG hostname as a domain target;
   DNS and policy routing remain on the proxy side.
3. A verified fake-IP or ePDG route on `utun` may use transparent Network
   Extension takeover. The UDP socket is not bound to a local IP or interface;
   Surge/mihomo must receive an ordinary system-routed socket.
4. When the active loopback system proxy belongs to Surge, the application
   checks Surge Enhanced Mode. It enables that mode before IKE when possible,
   because System Proxy alone does not carry UDP 500/4500. If activation cannot
   be confirmed, startup stops immediately with a manual-action message.
5. When no proxy transport is available, a real public ePDG address may use the
   current network directly. A `198.18.0.0/15` fake-IP is never treated as a
   public direct destination.
6. UDP 500, UDP 4500, NAT-T ESP and the userspace IMS data path remain on the
   selected transport for the whole session.

## Verified Surge result (2026-09-19)

With Surge System Proxy enabled but Enhanced Mode disabled, the session
stalled before IKE_INIT and timed out. After automatic Enhanced Mode activation
the same physical reader completed IKE fragmentation, EAP-AKA, Child SA,
userspace ESP, IMS AKA registration and reached SMS ready in about five seconds.

The 2026-09-21 macOS 27 / 1.14 release gate repeated the same path with the
manual proxy field empty: IKE/ESP was ready in 2.7 seconds, Rust PC/SC handled
both AKA transactions, the fallback P-CSCF completed protected REGISTER, and
IMS/SMS/call readiness stayed healthy under 30-second OPTIONS probes.

A second validation cycle completed the same automatic Surge path on the
physical CTExcel SIM. The registrar granted a 2400-second Contact binding, the
runtime selected the actual winning P-CSCF for status/routing, and scheduled a
full authenticated refresh at 35 minutes. A forced heartbeat timeout in the
preceding cycle was detected and recovered end-to-end in about six seconds.

The lifecycle decision is now made only by the Rust control plane rather than
mirrored there. Three controlled 2026-09-21 fault injections temporarily
disabled Surge Enhanced Mode after a healthy heartbeat. Each IMS probe timed
out at 12 seconds, Rust supplied the bounded reconnect delay, Surge Enhanced
Mode was restored automatically, and the physical CTExcel session returned to
tunnel/IMS/SMS/call ready. The third cycle completed recovery about nine seconds
after failure detection. This satisfied the three-cycle cutover gate, so the
duplicate Go delay calculation and silent fallback were removed; an unavailable
Rust policy is now an explicit session error.

## IMS binding lifetime

- Baseline defect: the network returned `Contact;expires=2400` while the
  top-level `Expires` echoed 3600; the runtime kept no REGISTER-renewal timer.
  OPTIONS could therefore remain healthy after terminating-call routing had
  expired.
- Target and measurement: use the shortest positive Contact grant, ignore
  stale contacts with `expires=0`, and rebuild the complete authenticated
  session with at least a one-minute safety window before expiry.
- Trade-off: an in-place protected re-REGISTER would avoid a short reconnect,
  but it currently duplicates security-state ownership. A bounded full-session
  refresh is lower risk and prevents false-online state now; native protected
  refresh is deferred until the Rust runtime owns the live IMS association.
- MT-call gate: `registered`, OPTIONS 200, and a returned
  `P-Associated-URI` are necessary but not sufficient. CTExcel inbound calling
  is accepted only when the network sends an actual terminating INVITE and the
  application completes alerting/answer/hangup. Routing to voicemail without a
  received INVITE remains a network-binding failure, not a UI success.

## Trade-off decision

Automatic Surge activation is deliberately narrow: it runs only when the
configured endpoint equals the active loopback SOCKS system proxy and the
official Surge CLI is present. It does not change Clash/mihomo, remote proxy,
or direct-network state. This bounded integration has a high benefit (restores
all UDP-based VoWiFi functions) and low implementation/runtime cost, so it is
kept. General-purpose proxy reconfiguration and profile rewriting are out of
scope because their global side effects exceed the expected benefit.
