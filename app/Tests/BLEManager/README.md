# Isolated BLE manager regression tests

Run `sh scripts/test-ble-manager.sh` from the repository. The test requires Ruby
and the Xcode Swift compiler, and does not launch the app or access Bluetooth.
Use `BLE_MANAGER_ITERATIONS=100 sh scripts/test-ble-manager.sh` to run each event
group 100 times in one process, with one compilation and fresh state per sequence.

`prepare.rb` reads the **current** `BluetoothReaderManager.swift` and
`BLEPhaseDeadline.swift` for every run. It keeps the full manager implementation,
including callback guards, Rust-issued phase/APDU deadline tickets, APDU framing/handling, and reconnect
logic. It removes the UI service wrapper and substitutes dependencies only:

- CoreBluetooth objects become in-memory adapters. Tests explicitly deliver
  discovery, notification, value, failure, and disconnect callbacks.
- Dispatch uses a serial virtual-time queue with cancellation; the production
  `BLEPhaseDeadline` schedules only the Rust-provided duration; stale ticket,
  phase and target checks plus timeout actions execute in `proletarius-ble-core`.
- UserDefaults and logging become memory/no-op adapters; no user state is touched.
- Combine publishing and private access control are removed for fixture setup
  and observation, without changing the state machine or APDU algorithms.

The suite covers delayed multi-service callbacks (including errors), duplicate
notification acknowledgements, 60-second healthy handshake APDUs, notification
loss after connection, reconnect without `didDisconnect`, superseded reader
callbacks, all four 30-second phase deadlines, manual cancellation, the existing
120-second APDU deadline, stale notification identity, and optional claim failure.
Callback-order expectations are assertions on the actual production methods;
the adapters do not implement reconnect or deadline policy.

This verifies deterministic application event handling, not macOS's radio stack,
real device timing, UI publishing, concurrent execution, or the Unix-socket bridge.
Remembered-reader UUID bytes come from UserDefaults, while the Rust target
owner decides whether a fresh advertisement may be selected and whether the
ready target may be persisted. The separate `test-ble-phase-deadline.sh` still
exercises real Dispatch scheduling.
For a source-snapshot comparison, set `BLE_MANAGER_SOURCE_DIR` to a directory
containing the two production Swift sources; the tests and adapters remain current.
