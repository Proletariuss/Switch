# EVS reference source provenance

Algorithm source: 3GPP TS 26.443 floating-point ANSI C reference implementation,
V16.3.0 (also labelled V12.14.0 / V13.10.0 / V14.6.0 / V15.4.0), 2021-11-04.
Specification: https://portal.3gpp.org/desktopmodules/Specifications/SpecificationDetails.aspx?specificationId=1465
Retrieved from https://github.com/old6ix/EVS_codec at
1bff9329ea91bc5a3ba869006051ae8223ddb3a9 (2026-10-07).

The `lib_com`, `lib_enc`, `lib_dec`, `encoder.c`, `decoder.c`, and `readme.txt`
files retain upstream attribution and notices. No upstream CMake code is used.
`switch_evs.c` / `switch_evs.h` are Switch's independent streaming adapter.
Build flags namespace the reference symbols `sub` and `Pow2` to avoid
collisions with the existing opencore AMR library.

Switch's local reference-source changes check CLDFB/FD-CNG allocations,
propagate encoder/decoder initialization failure, and release partially
initialized resources. The reference CLI and EvsRX callers check these
initialization results. DSP arithmetic, successful initialization parameters
and successful call order remain unchanged. A separate test archive can
instrument allocations; those test hooks are not in the production archive.

The mirror does not supply a standalone open-source LICENSE. Do not label this
reference code as MIT/BSD/GPL or imply a patent licence. Public specification
availability does not itself establish commercial codec patent rights.
