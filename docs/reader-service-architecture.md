# Reader service boundaries

The currently working PC/SC reader, VoWiFi, SMS and call paths remain in place.
This is an incremental boundary, not a claim that BLE card operations are live.

```text
Reader UI ─► BluetoothReaderService ─► BluetoothReaderManager ─► CoreBluetooth
          └► ReaderESIMStore / ReaderRuntimeStore ─► authenticated backend API
                                                       ├► CardService ─► readerManager / eSIM Manager
                                                       ├► CardDownloadService ─► download progress
                                                       ├► NetworkService / IMSService ─► status snapshots
                                                       └► Telephony ─► CallService / SmsService
```

`BluetoothReaderManager` owns scanning, connection, notification characteristic
discovery, disconnection, reconnect backoff, published state and stable error
codes. The reader UI only invokes `BluetoothReaderService`. A discovered or GATT-
connected device is **not** considered a card session. The app-owned
`ReaderBLEBridge` exposes a single-owner APDU channel over a token-authenticated
Unix socket. The Go card adapter selects it only for `BLE:<CoreBluetooth UUID>`
readers; USB PC/SC remains unchanged. Rust's reader inventory imports only a
validated BLE identity reported by the authenticated compatibility owner.
Opening the reader's Bluetooth sheet starts an unfiltered advertisement scan,
then filters for the first-generation `ESTKme-RED` name. The sheet has an
explicit rescan action. Once the bridge reports a powered card, the UI enables
the reader control and refreshes the shared reader inventory; disconnect also
refreshes it, so a stale BLE selection is removed. System Bluetooth inventory
alone does not prove the reader is absent because discovery requires an active
scan.

For ESTK RED2, GATT write mode is selected from the selected writer
characteristic's properties. Write Without Response is used only when ordinary
Write is unavailable; ordinary Write is sent one MTU-sized chunk at a time and
advanced only by its delegate acknowledgement. This follows NekokoLPA2's
`ble_transport_utils.dart` policy and avoids advertising support for a
write-with-response characteristic while transmitting with the wrong mode.

`CardService` provides card presence, identity and profile queries, and owns the
serialized APDU/profile-switch step with reader-generation fencing. The existing
profile-switch HTTP transaction still owns VoWiFi stop, progress reporting,
reader-session reset/rebuild, operator notification delivery and post-switch
ICCID confirmation. Keeping those lifecycle stages together in the route avoids
changing the already-working eSIM completion behavior while incrementally moving
the transport boundary.

`CardDownloadService` defines the ordered states PREPARING, AUTHENTICATING,
DOWNLOADING, INSTALLING, ACTIVATING, REGISTERING, COMPLETED and FAILED. The
existing reader download endpoint advances it from the eSIM manager's real
preflight, authentication, download and install callbacks. The normal download
operation ends after installation: it does **not** claim activation or IMS
registration. A plan can require those later stages for a different workflow.
The existing operation API remains authoritative for the UI.

`NetworkService` and `IMSService` read separate snapshots. The reader network
adapter does not fabricate cellular registration or VoLTE from a PC/SC/BLE
reader, which has no radio. `CallService` and `SmsService` depend only on the
`Telephony` interface.

`backend/cmd/proletarius-test-console` is a development-only Unix-socket
client. It requires `--execute` for changes, hides response bodies by default,
and never reads or stores the production app's ephemeral authentication token.
Start an isolated development backend before using it; do not run it against a
live production app. BLE scan/connect/disconnect/status are exposed through the
same authenticated backend; connecting is asynchronous, so poll status before
running card APDUs. A successful scan request alone is not a card test.

Structured operation logs contain timestamp, module, action, state, result and
errorCode. Never include APDU contents, ICCID/IMSI, phone numbers, activation
codes, IMS identities or SMS bodies in those logs.

## Next integration gate

1. When ESTKme-RED 2.0.3 is attached, verify the CoreBluetooth-to-Go APDU
   bridge, including claim, power-on, logical channel and USIM/ISIM selection.
2. The offline suite now fault-injects BLE disconnect, lost notification,
   timeout, write-mode pacing and profile-switch identity mismatch; those tests
   are not a substitute for hardware validation.
3. On hardware, smoke-test scan, connect, card read, profile switch and VoWiFi.
   SMS and bidirectional calling remain separate carrier-level acceptance
   checks and must not be inferred from card transport success.
