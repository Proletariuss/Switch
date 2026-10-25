# Embedded LPA data provenance

These resources were copied byte-for-byte from Switch's existing source tree
during iteration 511. Relocation does not change metadata estimates, certificate
contents, CI trust-policy selection, or certificate verification behavior.

- `reference_sizes_simple.json` came from the former
  `backend/internal/esim/data/reference_sizes_simple.json`, derived from
  NekokoLPA2 revision `013eadad7037248531f1b1532ebf344390d57087`. The full MIT
  notice (copyright 2026 Nekoko) remains in the repository's THIRD_PARTY_NOTICES.
  SHA-256: `0e44d4780a59005f95547f977a0a0a8b6455bf0bac9d60142e084aa595998744`.
- `rootci/bundle.pem` and `rootci/bundle-tests.pem` came from the former
  `backend/third_party/euicc-go/http/rootci/`, pinned to
  `v1.1.3-0.20260628013808-8d873a2dfc98`. Its full MIT license remains in
  `licenses/euicc-go-LICENSE`; historical upstream hashes are retained in
  `licenses/euicc-go-UPSTREAM.md` at the repository root.
  Production bundle SHA-256:
  `ecdeb31a04c8692acf48c8e47d50e11c09e8a2e03b92db0c34b57d8fd8a2564f`.
  Test-CI bundle SHA-256:
  `c2d331885c3e3070a92aed0b49465d7f18ab1aedfdb5bc210cf60e268e2ca2d6`.

Both CI bundles are embedded by the native ES9+ adapter. The test-CI bundle is
selected only by the existing trust policy; its filename is not permission to
delete it as an unused test-only asset.
