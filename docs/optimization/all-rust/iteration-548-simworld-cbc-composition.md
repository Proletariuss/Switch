# Iteration 548 — offline CBC session-to-ESP composition

2026-10-18. The current ESTKme-RED / SIM World 454/03 session negotiates
ESP AES-256-CBC with HMAC-SHA256-128. Existing component tests covered the
data-plane codec and an earlier GCM session composition, but did not validate
this exact suite from final EAP authentication through the first IMS TCP SYN.

The new test `auth/cbc_child_tests.rs` runs a synthetic final EAP exchange,
installs the CBC CHILD SA, asks the real user-space IP stack to originate a
TCP SYN toward the assigned P-CSCF, and captures the ESP datagram sent on the
same loopback NAT-T UDP channel. A frozen KEYMAT oracle was independently
generated with Python standard-library HMAC PRF+, not Rust's key derivation.
The test independently parses SPI, sequence, IV, truncated HMAC, decrypted
AES-CBC payload, ESP trailer and numbered padding; it validates the inner
IPv4/TCP endpoints, SYN flags and checksums without using the Rust ESP
receiver or smoltcp parser. SPI, sequence, IV, ciphertext and ICV corruption
cases are rejected. The production path is unchanged; AES/CBC/HMAC/SHA2 are
test-only dependencies already present in the lockfile dependency graph.

`cargo test --locked -p proletarius-vowifi auth::` passed 12/12, including the
new integration case. This narrows one possible local encoding/key-direction
regression. It is **not** an independent capture of the real SIM World
session's KEYMAT, nor proof that the proxy exit or ePDG receives/accepts ESP.
The real observation remains IKE/EAP/CHILD success followed by IMS TCP timeout
and ESP 8 sent / 0 received. A known-good same-card/same-network client
comparison or proxy-exit packet-class evidence is still needed before changing
the production transport based on this fault. No card, proxy, SMS, call or DMG
operation was performed for this test.
