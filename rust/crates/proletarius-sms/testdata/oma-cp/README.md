# Historical OMA-CP regression vectors

These synthetic inputs preserve the literal byte constructions and expected
semantic assertions from `backend/pkg/smscodec/wbxml_omacp_test.go` before its
retirement. `manifest.json` records the original source hash and each input
hash. No expected output was generated from Rust.

- `provisioning.wbxml`: `buildTestWBXML`; NAPDEF internet APN and Telekom
  APPLICATION with APPID w4.
- `short-wsp.bin`: `TestDecodeOmaCPFromTPDU_WithWSPHeader`; prefix 01 b0.
- `long-wsp.bin`: `TestDecodeOmaCPFromTPDU_WithLongWSPHeader`; prefix bytes
  01 through 18 hex, locating WBXML past the old 16-byte scan boundary.
- `encrypted.bin`: complete `TestDecodeOmaCPFromTPDU_EncryptedFallback`
  literal. It must fail parsing and surface the possibly encrypted diagnostic;
  this fixture does not establish an encryption algorithm or attempt decryption.
- `attribute-value-orphan.wbxml`:
  `TestDecodeWBXML_AttrValueTokenAmbiguityAndEmptyParmName`; NAME must be
  NAPDEF when 0xa4 is an attribute value, and the nameless orphan parm must
  not create an empty key.

Header validity and non-shortest multi-byte public ID cases are preserved as
literal assertions in `oma_cp.rs`. Structured expectations are tested through
the existing private Reader; no product API or parsing behavior was added.
