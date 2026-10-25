# Frozen protocol reference data

Iteration 511 preserves 297 protocol invocations before deleting the legacy Go backend. `protocol-manifest.json` records hashes, source revision plus dirty-source hashes, generation commands and provenance. `legacy-source-hashes.json` covers the actual Go source/module graph. Expected outputs were computed in Go without Rust callbacks or subprocesses; independent review matched every input table to its previous test.

The 4 PCMA rows include all 65,536 signed PCM inputs. AMR-NB contains 32 success vectors in both directions (64 invocations); these historical helpers do not enable PCMA or AMR-NB calling. AMR-WB contains 24 mode/alignment vectors, including SID and no-data. MESSAGE routing has 66 combinations; USSD10, SIP wire4, call target10 (4 explicit errors), rejection8, SDP admission57, and SMS submit50. Only the original SMS concatenation reference is normalized to42, after header validation.

SIP inputs include captured Go-generated headers, so that group proves serialization rather than independent native dialog composition. Call rejection strings are reviewed literals. SDP admission flags are reviewed literals from the old adapter test; they do not constitute an independent SDP negotiation oracle. Explicit EVS/all AMR-WB capabilities are used for admission; actual native codec availability and audio are tested by separate frozen audio vectors.

Run `scripts/test-frozen-protocol-fixtures.py` after sourcing `scripts/workspace-env.sh`, or use the normal migration gate. All files, counts, unique names and expected result kinds are validated before execution. Expected outputs are never regenerated during validation. No reader, card, SMS network or external call is opened by these examples.
