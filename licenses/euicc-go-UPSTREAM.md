# Pinned euicc-go framing patch

This directory is a small, self-contained copy of the existing dependency,
not a vendor tree of its transitive dependencies. Production imports and the
required version remain unchanged; `backend/go.mod` selects this local copy
with an explicit `replace`. No Go module-cache source file is patched.

## Source and license

- Upstream: https://github.com/damonto/euicc-go
- Pinned version: `v1.1.3-0.20260628013808-8d873a2dfc98`
- Commit: `8d873a2dfc98` (revision encoded in the pinned Go pseudo-version)
- License: MIT; original `LICENSE` and copyright are retained unmodified.
- Upstream module checksum: `h1:n2VA/WOWZCYjcd90zAuH+PlZpDjiEeXfIO1rMbIEsfM=`
- Upstream go.mod checksum: `h1:L2YqXCb1ESnL1ZlqkQ7t/bVrcqrgt1hCwdzWrVzgYN0=`
- Original `driver/transmitter.go` SHA-256:
  `bbb2b93d1db89ef065a40daa95127013cf6da1d63cdbad0c7c66616043121fd7`

The cached upstream module passed `go mod verify` before this copy was made.
The copy includes all 95 original files (about 305 KB of source bytes),
including upstream tests and certificate assets. It does not add another
runtime, upgrade dependencies, or copy unrelated Go modules.

## Local changes

Only `driver/transmitter.go` is changed from upstream. The additional
`driver/transmitter_boundaries_test.go` exercises the local patch.

1. Decide the final STORE DATA block by its byte offset plus data length,
   rather than comparing the byte-sized P2 counter with a quotient.
2. Preserve P2 increment/wrap modulo 256, CLA/logical-channel encoding,
   MSS=120 chosen by Proletarius, GET RESPONSE handling and stop-on-error.
3. Reject raw empty commands and MSS outside 1–254 explicitly. Constructor
   validation occurs before touching the card. The public LPA options retain
   their existing convention that an omitted/zero MSS normalizes to 254.

All production raw APDU requests are generated from nonempty BER-TLV
structures; no intentional empty STORE DATA caller was found. No transport
retry, profile-write replay or automatic fallback is added.

## Reproduce without hardware

From the Proletarius `backend` directory, with the existing locked dependency
cache available:

```sh
GOPROXY=off go test -race github.com/damonto/euicc-go/driver -count=1
GOPROXY=off go test github.com/damonto/euicc-go/bertlv/... github.com/damonto/euicc-go/lpa github.com/damonto/euicc-go/v2 -count=1
GOPROXY=off go test ./internal/esim -count=1
go list -m github.com/damonto/euicc-go
```

The last command must show `=> ./third_party/euicc-go`. No build overlay or
machine-specific flags are needed for normal builds, release packaging or
source distributions. Keep this directory with the backend source.

Do not use `go test github.com/damonto/euicc-go/...` as a macOS release gate:
upstream's unused Qualcomm QRTR drivers require Linux, and its unused `at`
driver does not compile against the host project's already-pinned newer
`uicc-go` API. Those unchanged upstream packages are not imported by the
Proletarius macOS build. No platform expansion or unrelated driver rewrite
is part of this framing patch.

The new regression reproduces the original bug at lengths 120/240 and the
255/256/257-block boundaries. The fixed result has exactly one final 0x91
marker on the last block, unmodified payload bytes and no APDU replay after
an injected transport failure. These are offline protocol tests, not proof
of successful installation on a physical 9eSIM card.
