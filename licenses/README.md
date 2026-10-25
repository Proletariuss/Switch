# Retained attribution

The license texts and `euicc-go-UPSTREAM.md` in this directory were copied
byte-for-byte from Switch's existing dependency snapshots during iteration 511.
Original component identities, copyright notices, and license terms are retained.
The production build copies the license files into the app's existing license
resource names; a component name ending in `-go` identifies its provenance,
not a bundled Go executable or a current build-tool requirement.

`euicc-go-UPSTREAM.md` is an unchanged historical snapshot record. Its paths,
Go reproduction commands, and present-tense implementation notes describe the
former source arrangement. The live CI-root resources now reside in
`rust/crates/proletarius-lpa/resources/rootci/`.

OpenCORE notices are retained for attribution. This directory includes no
OpenCORE source, static archives, or runtime payload. EVS and AMR-WB use the
separately preserved C reference source under `third_party/evs/`; its NOTICE
does not grant an open-source or codec patent license.
