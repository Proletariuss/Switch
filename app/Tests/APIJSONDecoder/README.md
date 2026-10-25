# Offline API response decoder check

Run from the repository with:

```sh
sh scripts/test-api-json-decoder.sh
```

An optional first argument chooses a separate output directory. The default is
`build/optimization/2026-09-26-sms-decoder`. The script compiles the actual
`APIClient.swift`, actual `SMSModels.swift` and this harness using
`-O -whole-module-optimization -swift-version 5`; no app, backend, socket, modem
or real message store is opened. Test-only URLProtocol rejects all requests.

The optimized path calls production `APIClient.decodeResponse` directly.
`BaselineDecoder.swift` freezes the previous date-decoding algorithm, identified
by its source SHA-256, solely for before/after comparison. Do not change that
baseline to match future production changes.

Fixtures contain 0, 100 and 1000 synthetic messages, up to 25 synthetic senders,
mixed fractional-second lengths and UTC offsets. Correctness checks compare all
SMS fields, existing computed IDs, grouped content and ordering within each
conversation. Separate cases cover 0–9 fractional digits, positive and negative
offsets, leap day, malformed dates, missing/null/wrong-type timestamps and
malformed JSON; error kind, coding path and debug description must match the
baseline. Thirty-two simultaneous calls exercise independent response decoders.

Each of five operations receives six warmups and forty measured samples per
fixture. Rotated/reversed operation order reduces systematic run-order bias.
Times use a monotonic clock and include autorelease-pool cleanup. Median is the
average of the two central samples; p95 uses nearest rank (38th of 40). All raw
samples are retained in `results.json`; environment, flags and production-source
hashes are in `environment.txt`.

The production timing includes new JSONDecoder and formatter creation. The
baseline decoder is pre-created, excluding APIClient/URLSession setup, making
the comparison conservative about response setup. Group-only timing uses the
same algorithm as SMSView against predecoded records. It is not a SwiftUI render
benchmark. IPC, hardware latency, polling cadence, notifications, actor
scheduling, real app CPU, memory and energy use are outside this measurement.

The candidate gate requires p95 reduction of both 25% and 50 ms. The harness
records this gate per fixture rather than failing correctness tests on noisy or
small-data timings. A successful process exit means correctness checks passed;
adoption also requires reviewing the performance evidence and maintenance cost.
