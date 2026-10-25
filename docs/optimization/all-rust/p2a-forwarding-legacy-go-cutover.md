# P2A: retire the duplicate Go forwarding runtime

This branch's P2A slice updates Telegram's Rust provider to resolve the System
Proxy/PAC route again immediately before each chunk submission. It also removes
the stale Go forwarding controller and `/api/sms/forwarding/*` handlers from the
compatibility child. The Rust gateway already owns and dispatches these public
paths to `sms_forwarding::Service` before Go fallback, so the public route
contract and Swift API stay unchanged.

No changes are requested to `proletariusd/src/main.rs`, `ownership.rs`,
`public_http.rs`, or the generated contract JSON. If contract verification
shows a missing Rust dispatch or an unexpected API compatibility dependency,
stop this cutover and bring that exact integration request to the integration
owner rather than editing shared route declarations in this branch.

Validation is offline and synthetic: provider tests use loopback proxy fixtures;
Go handler tests verify the old child no longer exposes these endpoints; Rust
forwarding, system-proxy, and ownership tests verify the service and its declared
owner. The separate API-contract scanner remains blocked by a stale Iteration143
review-scope entry for `module_profile_notes.handle`; this branch did not change
the shared review or contract files. No real provider, SMS body, secret, card,
or hardware is accessed.
