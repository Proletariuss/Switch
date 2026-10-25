# Historical Bound Profile Package segmentation fixtures

Copied byte-for-byte from the pinned euicc-go v2 fixtures before Go retirement:
`v1.1.3-0.20260628013808-8d873a2dfc98`. Original test:
`TestSegmentedBoundProfilePackage` in `v2/bpp_test.go`. Original MIT license
and copyright remain in repository-root `licenses/euicc-go-LICENSE`.

The four original fixture names identify Infineon, Redtea Mobile, Tigo and
Tele2, in that order. `bpp@N.txt` contains base64 package input;
`sbpp@N.txt` contains comment lines and expected segment bytes in hex.
These existing expected files are the oracle, not native Rust-generated
outputs. Source/test and fixture hashes are recorded in `manifest.json`.
No profile is installed, network requested, or real card opened by the test.

`tests/historical_bpp.rs` passes decoded inputs through the production
`Euicc::load_bound_profile_package` path using an in-memory card transport.
It reassembles STORE DATA APDU chunks at the final-chunk bit and checks every
segment plus total segment count against each original expected file. Both
MSS 254 and 37 exercise APDU chunking without changing logical segmentation.
A separate small package and its seven literal expected segments preserve
`TestSegmentedBoundProfilePackageSplitsSequenceOf87`, including splitting the
two adjacent 87 TLVs. Expected vectors are never derived by the Rust encoder.

Historical loader detail: `bpp@2.txt` and `bpp@4.txt` each end with two
surplus `=` characters after a complete canonical base64 package. Go's
`LoadBoundProfilePackage` calls `TLV.ReadFrom` over a streaming decoder; its
length-limited constructed TLV reader stops after one package without
draining the trailing decoder input. The Rust test derives the exact package
length from the BF36 BER header, strictly decodes only the corresponding
base64 quantum prefix, and asserts that the remainder is exactly `==` for
those two named fixtures and empty for the other two. It does not remove
arbitrary trailing content, alter the fixture, or relax segment expectations.
