# Iteration 553: optional 3GPP IKE device identity parity

The archived Go 2.86 daemon registered SIM World / 3HK in the controlled
registration-only comparison, while the previous Rust daemon established its
CHILD SA but received no ESP reply to its first IMS TCP attempts. The archived
Go binary contains the `NewIMEIDeviceIdentityNotify`,
`buildIKEAuthInitPayloads`, and `deviceIdentityIsEnabled` functions; the
archived source adds an optional DEVICE_IDENTITY notify in its first IKE_AUTH.
No decrypted successful-session IKE_AUTH packet was captured, so this source
and binary evidence does **not** prove that the notify caused the difference.

The Rust first IKE_AUTH now emits exactly one type 41101 notify when the
carrier's device-identity switch is enabled and the existing frozen session
has a valid 15-digit ASCII IMEI. A configured carrier IMEI takes precedence
over the same persistent app IMEI already used for IMS; explicit
`device_identity_enabled: false` suppresses the notify. Missing or malformed
optional input does not create a new IKE builder error. Existing session
preparation still validates required IMEI with Luhn and reports its existing
error before network startup. No IMEI appears in logs or error diagnostics.

The notify body matches the deployed Go fixture for the synthetic IMEI
`351234567890124`:

```
00 00 a0 8d 00 09 01 53 21 43 65 87 09 21 f4
```

This is the Notify payload body: protocol 0, SPI size 0, notify type 41101,
identity data length 9, identity type 1, and fifteen TBCD digits. It is sent
only inside the encrypted initial IKE_AUTH. Subsequent EAP/AUTH messages,
IMS REGISTER, ESP proposals, SIM authentication, and proxy routing are
unchanged. In particular, Rust still does not advertise MOBIKE or session
resumption without handlers.

Offline verification: `proletarius-vowifi` `auth_config::tests` (4 passing),
`proletariusd` `native_vowifi_session::configuration::tests` (12 passing),
including enabled, disabled, override precedence, absent/malformed input,
and the Go byte fixture. This does not establish real 3HK registration or
explain the observed zero ESP receive count. A subsequent device test should
compare old/current under the same card, ePDG, proxy policy, and selected
P-CSCF, changing only the Rust 41101 presence and recording payload-free
notify-type and packet counters. No SMS, call, profile mutation, or direct DNS
bypass is part of that test.
