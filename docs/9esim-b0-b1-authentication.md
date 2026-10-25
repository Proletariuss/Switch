# 9eSIM B0/B1 authentication in Switch

## What is implemented

- **B1 BLE password authentication:** `app/Sources/BluetoothReaderManager.swift`
  identifies the Nordic UART service, derives the default password from the
  device suffix, completes the encrypted challenge/response, checks the
  authentication acknowledgement and eUICC management mode, and withholds the
  APDU-ready state until those checks pass. APDU frame counters, checksum,
  response validation, and idle keepalive remain part of the same session.
  Per-device custom Bluetooth passwords can be set from the BLE reader list and
  are stored locally in the macOS Keychain under a reader-specific account. A
  saved password takes precedence; removing it restores the device-derived
  default. An explicit rejection or credential-store error stops automatic
  reconnect until the user retries. The password is never written to diagnostics.
  The private APDU/reset bridge requires the authenticated, card-mode-verified
  connected state even for compatibility requests that omit the reader ID.
- **B0 encrypted download authentication:** `rust/crates/proletariusd/src/b0_auth.rs`
  applies only to the supplied EID range. It fetches a 16-byte PSK from
  `https://key.9esim.com/psk` when absent from the macOS Keychain, generates a
  fresh 16-byte nonce, derives the BF71 MAC using AES-128-CBC with zero IV and
  no padding, then transmits `81 E2 91 00` plus the 27-byte BF71 TLV over the
  already-open eUICC logical channel.
- The B0 hook runs after `GetEUICCChallenge` and immediately before the
  standard `AuthenticateServer` card command. Only `9000` allows the normal
  download to continue. Missing/malformed PSK, unsupported challenge length,
  transport errors, or any other status fail closed and cancel the LPA session.
- The B0 PSK is cached under Keychain service `clarivate.nine-esim` and account
  `b0-psk:<EID>`. EID is sent to that HTTPS endpoint only for cards in the
  configured EID range. No EID, PSK, nonce, or challenge is written to logs.

## Verification

- Rust unit vector matches the independent OpenSSL result supplied with the
  Clarivate archive: `BF71 18 80 04 06A18B06 81 10 202122...2F`.
- LPA regression proves the vendor hook runs before `AuthenticateServer`, and
  that a hook failure prevents `AuthenticateServer` from being sent.
- eUICC APDU test verifies logical-channel CLA `81`, instruction `E2 91 00`,
  exact 27-byte TLV payload, and rejection of malformed payload lengths before
  touching the transport.
- These tests validate synthetic protocol framing and cryptographic behavior.
  They do not constitute a B0 real-card authentication/download or B1 new-build
  hardware test. B0 APDU acceptance and profile download still require the
  target B0 card and a currently authorized test profile.

## Protocol boundary

The B1 Bluetooth challenge password and B0 BF71 download PSK are different
credentials and different authentication layers. They use separate Keychain
accounts (`ble-password:<CoreBluetooth device UUID>` and `b0-psk:<EID>`) under
the same service. The supplied Clarivate archive explicitly treats the BLE
transport password as unrelated to BF71. B1 keeps the device-derived password
as its default and permits a per-device custom override.

The code and synthetic protocol tests cover both authentication layers. They
do not establish that a new DMG has completed a real B0 download or a B1
hardware authentication; those still require the corresponding physical card.
