# Third-Party Notices

## Rust serial port access

The Rust-owned modem serial port uses `serialport` 4.10.1
(https://github.com/serialport/serialport-rs), licensed under the Mozilla
Public License 2.0. The crate is an exact-pinned Cargo dependency with default
features disabled. Its license text is available at
https://mozilla.org/MPL/2.0/ and in the crate's `LICENSE.txt`.

## Rust userspace IP stack

The native VoWiFi IP-device integration uses smoltcp 0.14.0
(https://github.com/smoltcp-rs/smoltcp), with its 0BSD license:

```text
Copyright (C) smoltcp contributors

Permission to use, copy, modify, and/or distribute this software for
any purpose with or without fee is hereby granted.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN
AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT
OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

Proletarius contains code derived from the upstream VoHive project and retains the license and required notice provided in the repository root [`LICENSE`](LICENSE):

```text
Required Notice: Copyright iniwex5 (https://github.com/iniwex5/vohive)
```

## Celmux AT session adaptation

The bounded AT command/reply transaction lifecycle in
`rust/crates/proletariusd/src/modem_actor.rs` is adapted from the AT session
implementation in Celmux (`crates/at/src/session.rs`,
https://github.com/anti-rainer/celmux-release). It uses the same PolyForm Noncommercial
1.0.0 license boundary. The adaptation keeps Switch's existing serial actor
as the sole port owner and preserves Switch-specific priority scheduling,
URC callbacks, SMS generation checks and watchdog behavior in the compatibility
layer. The source attribution required by Celmux is:

```text
Required Notice: Copyright iniwex5 (https://github.com/iniwex5/vohive)
Required Notice: Celmux modifications Copyright 2026 anti-rainer (https://github.com/anti-rainer/celmux-release)
```

The VoHive notice is also retained in the root `LICENSE`. Celmux's local
reference snapshot was `/Users/proletarius/Downloads/celmux-src/`; it is not
copied wholesale into this source tree. The AT session remains a behavior-level
adaptation and no AT crate files were copied. Selected MIT eUICC protocol
modules were copied on 2026-10-05; those files and their independent MIT terms
are recorded below.

## Celmux eUICC and LPA protocol code (MIT)

`rust/crates/proletarius-ccid/src/`,
`rust/crates/proletarius-euicc/src/`, and the transport-independent state
machine in `rust/crates/proletarius-lpa/src/` reuse the portable APDU,
BER-TLV, logical-channel, ES10c profile-management, Bound Profile Package,
activation-code, and SGP.22 LPA-flow code from
`/Users/proletarius/Downloads/celmux-src/crates/euicc/{ccid,euicc,lpa}/src/`.
The Rust source headers retain `Copyright (c) 2026 anti-rainer` and
`SPDX-License-Identifier: MIT`; crate references were renamed from
`celmux_ccid` to `proletarius_ccid` for the Switch workspace. The reader APDU
adapter in `rust/crates/proletarius-reader/src/euicc_transport.rs` is Switch
code and does not acquire a PC/SC handle or run a card operation by itself.

MIT-licensed protocol sources and a transport-independent LPA state machine
were copied/adapted. Selected QMI wire, client-core, and IMS/IMSA codec files
are also reused under their file-level MIT notices. Celmux's PolyForm
application, AT implementation, platform QMI transports, manager, IMS/VoWiFi
runtime, network, and UI crates are not copied. Its ES9+ HTTP client was reviewed but not copied;
`rust/crates/proletarius-lpa/src/es9_http.rs` is a Switch-owned adapter informed
by Celmux's MIT protocol mapping. It preserves Switch's existing GSMA CI root
bundle, Admin Protocol 2.5.0, direct route, response limits, and no-redirect
behavior; TLS verification stays enabled. It has offline protocol tests but is
not connected to a production eSIM route or reader owner yet. The existing
GSMA CI root PEM files remain sourced from the MIT-noticed euicc-go copy below.
The Rust profile-metadata parser and size-estimate adapter are Switch-owned and
follow the existing Go behavior; the embedded reference table is the already
noticed NekokoLPA2-derived data documented below. No Celmux size-estimation
implementation was copied. Selected MIT SIP parser, dialog, and transaction
files are recorded in the following Celmux SIP section; Celmux's IMS/VoWiFi
application and transport runtime were not copied.
This work does not implement or claim to resolve the explicitly excluded 9eSIM
`6982` case. The complete MIT terms applicable to the adapted source follow in
the Celmux reuse license block below.

The call-mode USB/ADB design was also informed by the PolyForm Noncommercial reference repository [`rogerbush007-a11y/DJOneHub-mac-enhanced`](https://github.com/rogerbush007-a11y/DJOneHub-mac-enhanced), inspected at revision `f6ee6aa5ba5d1ca13ef65956adda7bc098c2fb92`. It carries the same upstream required notice and is not embedded as a binary dependency.

Legacy `QADBKEY` interoperability was informed by the public GPL-3.0-licensed [`carp4/qadbkey-unlock`](https://github.com/carp4/qadbkey-unlock) reference at revision `cab52a0a7429c8d8b8f31da8894c8c93155c0fc5` and independently implemented using the documented Unix MD5-crypt format. The current Rust implementation is in `rust/crates/proletariusd/src/qadbkey.rs`. Proletarius does not bundle or execute that Python script; the compatibility implementation derives the password locally in memory and never persists it.

## ESTKme-RED BLE protocol reference

The ESTKme-RED frame format and GATT UUIDs used by the macOS Bluetooth reader adapter and discovery UI were adapted from [NekokoLPA](https://github.com/iebb/NekokoLPA), by Kosmoneko OÜ (2024) and Infineon Technologies AG (2022), under the MIT license. Its required copyright and permission notice is retained below:

```text
Copyright (c) 2024 Kosmoneko OÜ
Copyright (c) 2022 Infineon Technologies AG

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.
```

The upstream source is not bundled as an executable dependency.

## NekokoLPA2 profile-size data and 9eSIM NBLE protocol

`rust/crates/proletarius-lpa/resources/reference_sizes_simple.json` is derived from
NekokoLPA2 revision `013eadad7037248531f1b1532ebf344390d57087` and is used
only to display a fallback estimated profile size when the SM-DP+ metadata
does not include one. The data is distributed under the MIT license:

The B1 9eSIM NBLE transport in `app/Sources/BluetoothReaderManager.swift` was
adapted from the NBLE implementation in the user's local NekokoLPA2-based
`Proletarius-iOS` working tree, whose checked-out base revision is
`013eada`. This adaptation covers the Nordic UART service mapping, derived
authentication handshake, APDU framing, response validation, and idle
keepalive. The B0 BF71 download-authentication flow in
`rust/crates/proletariusd/src/b0_auth.rs` follows the supplied Clarivate
2.2.29 source excerpt, including EID selection, PSK retrieval/cache, and the
pre-AuthenticateServer card command. It does not bundle the iOS app or its UI.
The same MIT notice below applies to these protocol adaptations and the
profile-size data.

The upstream required copyright and permission notice follows:

```text
Copyright (c) 2026 Nekoko

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Release Runtime

The macOS release package includes **libusb 1.0.30**, distributed under the GNU Lesser General Public License, version 2.1 or later.

- Project: <https://libusb.info/>
- Source: <https://github.com/libusb/libusb/releases/tag/v1.0.30>
- License text in the release package: `licenses/libusb-COPYING`

## Optional Module Voice Runtime

Proletarius does **not** include the QDC507 module voice runtime in its source tree or release package. When a user explicitly confirms the download in the call page, the application downloads the files from an immutable MaVo revision, either directly from GitHub or through a user-selected HTTPS relay, then verifies fixed file sizes and SHA-256 hashes before saving or deploying anything.

- Project: <https://github.com/moluncn/mavo>
- Exact source revision: `0443dfdaf8aec086fd76ba2ee9152fd908114524`
- Upstream runtime directory: <https://github.com/moluncn/mavo/tree/0443dfdaf8aec086fd76ba2ee9152fd908114524/Resources/ModuleVoice>
- Local download directory: `~/Library/Application Support/Switch/voice-runtime/qdc507-3.18.44-voice-20260712.5`
- Downloaded license copy: `COPYING-GPL-2.0`
- Downloaded build/safety report: `MODULE-REPORT.md`

The downloaded helper `mavo-pcm-bridge.armv7` is part of the MIT-licensed MaVo project. The two downloaded kernel objects expose `MODULE_LICENSE("GPL v2")` metadata and are accompanied upstream by the GPLv2 text. Proletarius neither mirrors nor republishes these artifacts; relay services are transport alternatives and are not treated as trusted publishers. This notice describes the application behavior and does not replace the upstream license materials.

Switch includes its own Rust `switch-voice-supervisor.armv7` inside the Rust
daemon. This small module-side parent starts the separately downloaded MaVo
helper and accepts authenticated session control; it does not include the MaVo
helper, kernel objects or vendor audio libraries. Its static ARM soft-float build
uses the Rust 1.98.0 standard-library component, whose full MIT/Apache license
texts and copyright notice are preserved under `licenses/rust-std-*` and copied
into the application. `scripts/prepare-module-supervisor.py --fetch` prepares
that hash-pinned build dependency within this workspace; subsequent builds
validate and use it offline. Cross compilation and ELF inspection alone do not
establish interoperability with a physical module.

The exact license texts for its Cargo runtime dependencies (HMAC, SHA-2,
digest, block-buffer, crypto-common, generic-array, typenum, subtle, cfg-if and
libc), plus the build-only version_check crate, are retained with a hashed
manifest under `licenses/module-voice-supervisor/` and included in the app.

## Retained dependency attribution

The Rust-native application retains attribution for data and protocol adaptations
from the former Go dependencies. Their original copyright notices and full license
texts are preserved byte-for-byte under `licenses/` and copied into the app's
`Contents/Resources/licenses/` directory. These notices do not identify a Go
runtime dependency. Native EVS reference C sources remain under `third_party/evs/`
with their separate provenance and licensing boundary described below.

| Component | License file |
| --- | --- |
| euicc-go | `licenses/euicc-go-LICENSE` |
| euicc-go BER-TLV | `licenses/euicc-go-bertlv-LICENSE` |
| uicc-go | `licenses/uicc-go-LICENSE` |
| quectel-qmi-go | `licenses/quectel-qmi-go-LICENSE` |
| swu-go | `licenses/swu-go-LICENSE` |
| opencore-amr | `licenses/opencore-amr-LICENSE` and `licenses/opencore-amr-NOTICE` |

The retained euicc-go CI-root data came from the copy pinned to
`v1.1.3-0.20260628013808-8d873a2dfc98` and now resides in
`rust/crates/proletarius-lpa/resources/rootci/`. The historical STORE DATA
framing correction and upstream hashes are preserved unchanged in
`licenses/euicc-go-UPSTREAM.md`; its Go commands describe that former source
snapshot, not the current build. The eUICC interoperability review used
NekokoLPA2 revision `013eadad7037248531f1b1532ebf344390d57087` as a behavioral
reference; its Flutter runtime and art assets are not embedded.

The `uicc-go` attribution retains its MIT license, copyright (c) 2026
Damon, in `licenses/uicc-go-LICENSE`. The complete license text is
also included in the app at `Contents/Resources/licenses/uicc-go-LICENSE`.

The historical swu-go adaptation added an optional `Config.OnReauthStart`
callback in `pkg/swu/config.go` and `session.go`, with lifecycle fixtures in
`external_reauth_test.go`. It marked planned IKE reauthentication before tunnel
teardown; a nil callback preserved the existing behavior. These historical
implementation notes preserve provenance; the native runtime does not execute
swu-go. The upstream copyright and license remain unchanged.

Switch 2.80 also hardened the former vendored user-space ESP receive window and
sequence exhaustion handling in `pkg/ipsec`, kept AES-GCM nonce construction
separate from borrowed key storage in `pkg/crypto`, and retired SWu send loops
on sequence exhaustion. Local regression fixtures covered these changes. No
upstream cipher negotiation or license terms were changed. A concurrency regression also led to
synchronizing the shared logger's publication, replacement and snapshots in
`pkg/logger`; its existing initialization and injection semantics are retained.

Retirement of Go source and execution does not remove the notices applicable to
retained or adapted material. This file is informational and does not replace
any component's full license text.

## RustCrypto CBC and block-padding

cbc 0.2.1 and block-padding 0.4.2 are used under the MIT license option.

Copyright (c) 2018-2022 RustCrypto Developers
Copyright (c) 2018 Artyom Pavlov
Copyright (c) 2018-2025 The RustCrypto Project Developers

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.

## Celmux reused protocol MIT license

Copyright (c) 2026 iniwex5
Copyright (c) 2026 anti-rainer

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.

### Celmux QMI wire codec

`rust/crates/proletarius-qmi-wire/src/lib.rs` reuses the transport-independent
QMUX/QRTR framing and QMI TLV codec from
`/Users/proletarius/Downloads/celmux-src/crates/qmi/wire/src/lib.rs`.
The source file retains its iniwex5 and anti-rainer copyright and MIT SPDX
headers. The reused layer is `no_std` and has no device-I/O dependency;
Switch-owned golden tests compare QMUX and CTL encoding with the existing Go
QMI wire contract. Celmux's QMI transports, other service codecs, client
manager, and device manager are not included here. The MIT terms above apply.

### Celmux QMI IMS/IMSA/NAS/UIM service adaptation

`rust/crates/proletarius-qmi-services/src/lib.rs` adapts the portable IMS and
IMSA identifiers/TLV maps from
`/Users/proletarius/Downloads/celmux-src/crates/qmi/services/src/ims.rs` and
the NAS Get Operator Name query/field map from
`/Users/proletarius/Downloads/celmux-src/crates/qmi/services/src/nas.rs`, and
the read-only UIM file/query field layouts from
`/Users/proletarius/Downloads/celmux-src/crates/qmi/services/src/uim.rs`.
These files are from the Celmux QMI services crate declared MIT. The Rust parser keeps
Switch's current Go response surface where it differs from Celmux (for example,
Go IMS Get does not surface call mode), and preserves the NAS operator-name
preference: use service-provider name first, then operator string name. Tests
pin these behaviors to `quectel-qmi-go`. The IMS source copyright/SPDX
attribution is retained in the Rust file and the MIT terms above apply.
`src/client.rs` is Switch-authored glue over the reused QMI core and codecs. It
models per-service client-ID allocation, response checking, close, and rollback
if a later service cannot open; fake-modem tests exercise IMS/IMSA and NAS
lifecycles. `src/uim.rs` uses the UIM session/file/read TLV shapes and attribute
record layout while keeping the EF-OPL → IMSI/EF-AD lookup order from Switch's
Go backend. The adapted UIM source snapshot has SHA-256
`9c8f74ffc2e78ef0d57c5ef2dfe559502581d60ced815d140ee0db35f5b8e4a5`.
These codecs and clients are not called by `switchd-rs` yet, so this does not
represent QMI device-owner migration.

### Celmux QMI client and transaction core

`rust/crates/proletarius-qmi-core/` reuses the MIT-licensed, transport-agnostic
client transaction and indication machinery from
`/Users/proletarius/Downloads/celmux-src/crates/qmi/core/src/`: `lib.rs`,
`fanout.rs`, `qmap.rs`, and `transport/mod.rs`. Source-file copyright and SPDX
headers are preserved; imports now target Switch's `proletarius-qmi-wire`,
The Linux proxy and Windows USB transport modules are omitted, and the
Celmux logging dependency was removed. The generic `Transport` seam remains
caller-owned. Three fake-transport lifecycle tests cover CTL client-ID
allocation/release, out-of-order concurrent transaction replies, and late
responses after timeout. Switch's separate
`rust/crates/proletarius-qmi-libusb/` implements its own macOS device filter,
interface claim, CTL_SYNC check, and bounded bulk transport; no Celmux
platform transport was copied. The Switch transport has only offline selector
tests and has not been connected to a physical modem or `switchd-rs`, so it
does not represent production device-owner migration. The MIT terms above
apply.

### Celmux QMI owner-recovery policy reference

`rust/crates/proletariusd/src/qmi_enhancement.rs` contains Switch-authored
recovery glue informed by the error classification and service-reopen
lifecycle in Celmux's MIT QMI manager, specifically
`/Users/proletarius/Downloads/celmux-src/crates/qmi/manager/src/lib.rs`
(SHA-256 `19fa17a007f0ee4de882570cc306efa373b4e53608911fb875ce2c9c29f4a690`)
and `services.rs` (SHA-256
`75a0fc7888d7fad6467dbdee8b8fb9824367232bcbf79661e2c0d84c008cbb5c`). No
manager source was copied. The adaptation is deliberately narrower: one
process-wide IMS/NAS/UIM control owner, typed requests only, bounded
reconnection, and no replay of a mutating request after an uncertain result.
The Celmux manager remains a read-only reference; the MIT license text above
is included with the other reused QMI components.

## Rust libusb bindings

The macOS QMI transport uses `rusb` 0.9.4, a Rust wrapper around the system
libusb library. `rusb` is distributed under the MIT license; its source and
license are available from <https://github.com/a1ien/rusb> and the locked
Cargo dependency. The runtime libusb 1.0.30 license is recorded separately
above.

Copyright (c) 2015 David Cuddeback
Copyright (c) 2019 Ilya Averyanov

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

### Celmux SIP protocol core

The `proletarius-sip` Rust crate reuses selected transport-independent source
files from `anti-rainer/celmux` (`crates/sip/core/src/lib.rs`, `dialog.rs`,
and `transaction.rs`). The new `proletarius-sip-runtime` crate also adapts
`crates/sip/runtime/src/lib.rs`, which is marked MIT at the source-file level.
Its import now targets `proletarius-sip`; it owns only bounded transaction
storage and timer scheduling and opens no socket or thread. Each copied file is
marked MIT. The crates omit
`auth.rs`, whose file-level license is BSD-2-Clause, because Switch already has
its own IMS AKA/digest implementation and the BSD file is not needed for this
SIP parser/dialog/transaction foundation. The source-file copyright and SPDX
headers are retained. The MIT terms above apply to the reused source.

### Carrier profile data provenance and VoCat reference review (2026-10-05)

Switch's current embedded 855 carrier selectors and 685 profiles originate from
[iniwex5/ios-carrier-profiles](https://github.com/iniwex5/ios-carrier-profiles/tree/cdba73e551810681f16b4cce0693c70e9e216c37),
pinned at `cdba73e551810681f16b4cce0693c70e9e216c37`, matching the source YAML in
`data/carriers/` and `rust/carrier-registry.json`. That project identifies
its source as MIT licensed, copyright (c) 2026 iniwex5. The full license notice
is preserved in `data/carriers/ios-carrier-profiles-MIT.txt`.
The complete notice is also included in the app at
`Contents/Resources/licenses/ios-carrier-profiles-MIT.txt`.
The earlier bundled dataset was pinned at
`29f5e2f42c532781248400cf9f2b34ecfc3a16b4`. The comparison source used on
2026-10-05 was pinned at `1751fd3d3629927ae8499aa9d682130c025a965d`; that audit
did not change production YAML. The later dataset refresh history is retained
in `data/carriers/SOURCES.md`.

[VoCat](https://github.com/MengMengCode/VoCat/tree/0dec0cbfa3445695e44a5108d88db399f639fccb)
was reviewed at `0dec0cbfa3445695e44a5108d88db399f639fccb` for compatibility
research. Its root license is Research & Evaluation; this review does not
relicense it as MIT. No VoCat runtime or configuration collection was copied
into Switch's production registry. Independently authored synthetic boundary
fixtures and offline audit code record findings without asserting real-carrier
verification. Raw research references remain outside the source tree.

### User-supplied 3 HK / SIMworld configuration (2026-10-07)

`data/carriers/user_templates/3_hk.json` is a configuration
file supplied by the user, who identified
[voorz/vohive-release](https://github.com/voorz/vohive-release) as its origin.
It is preserved unchanged as a narrowly scoped overlay. Protocol semantics were
reviewed against `voorz/vowifi-core` v1.4.5 and its SWu dependency v0.3.8.
No implementation from those references is copied into Switch, and their
public availability is not treated as a license grant. The existing Switch
protocol libraries implement the required interoperability behavior.

## EVS reference codec

EVS Primary WB 13.2 kbit/s uses the 3GPP TS 26.443 V16.3.0 floating-point
reference C implementation. Source provenance and attribution are retained in
`third_party/evs/NOTICE.md` and `readme.txt`, also bundled in the app.
Switch's streaming/RTP adapter is separate from the DSP algorithm code.
Local reference-source changes add allocation checks, initialization failure
propagation and partial-resource cleanup; successful DSP arithmetic and
initialization parameters/order are preserved. The provenance notice lists
this modification boundary. Allocation instrumentation is confined to a
separate test archive and is absent from the production codec archive.
The reference mirror supplies no standalone open-source licence; this notice
makes no representation of codec patent licensing.

## RustCrypto DES / Triple DES

The Rust IMS security transport uses `des` 0.9.0 under the MIT license option.
Upstream: https://github.com/RustCrypto/block-ciphers/tree/master/des

Copyright (c) 2017-2024 The RustCrypto Project Developers
Copyright (c) 2017 Gulshan Singh, Antoni Boucher, Artyom Pavlov

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
