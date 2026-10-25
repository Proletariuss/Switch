# Module radio owner contract — Iteration096

Latest reviewed extension: Iteration480 native cellular call control, state, original serial/USB observation and independent CS history source; Iteration479 call-mode lifecycle and serial close fencing remain applicable. Generic Go eSIM/IMS/VoWiFi callers retain their existing contracts; native cellular and call-mode identities are allocated in-process by Rust.

Status: implementation contract, not hardware acceptance. All tests use synthetic transports. This contract replaces Go's flight-mode preferences writer, policy timer, RF transitions and VoWiFi radio restoration; Go retains caller lifecycle adapters while those callers migrate.


## Iteration480 reviewed cellular boundary

All five canonical public cellular call paths now dispatch to the in-process `ModuleCallService`; the five private Go call-control adapters and four duplicate Go public registrations are removed. Go no longer owns CLCC/URC state, pending dial/revision, CS radio reservation scheduling or the CS call-facts source. Original serial and USB lines enter one producer/epoch/session-fenced native observer before compatibility delivery. The `call_event_owned` frame field suppresses only Go call callbacks, retaining SMS/SIM/USSD/PCM and explicit legacy/mock behavior.

| Responsibility | Current authority / required completion |
| --- | --- |
| Status and pending dial | `module_call_state.rs` serializes URC/query/command transitions; stale revisions cannot end a new call, CLIP cannot create RF ownership, and early CONNECT remains bound to the accepted dial ticket |
| Physical action | `module_radio/call.rs` and serial/USB plans retain the exact producer, epoch, reservation and physical session; fixed CLCC/ATD/ATA/ATH/VTS recheck final admission after USB drain/before actual write, never replay uncertain submission |
| Radio reservation | Native retained desired worker holds one identity; unconfirmed release leaves the pointer fenced and prevents new dial reuse; callbacks enqueue metadata without durable I/O |
| CS history | Independent native daemon producer and bounded `call_facts/native.rs` lane retain immutable sequence/timestamp/head until durable ACK; source retirement follows real physical release and worker shutdown |
| IMS compatibility | Go keeps two ordered IMS fact sources and SIP/media/session owners. Private `history-error` accepts only current Go producer plus module_ims/reader_ims; bounded metadata retry does no physical action, sticky faults never clear, transient errors clear only after real recovery |
| Public history errors | Native CS status reads the complete CallFacts aggregate; two Rust IMS aliases merge it into validated Go IMS observations, preserving local Go error if native has no aggregate |
| Transport retirement | Native accepted jobs and actual release must settle before replacing/closing USB or serial; final shutdown closes observations, joins reservation worker, then retires/drains CS facts. It does not issue an automatic hangup |

Generic raw serial/USB writes reject cellular control while the native observer is installed, including Hayes compound call commands and unquoted VTS. Historical Go CS callback/reservation descriptions below are superseded by this section. Go `moduleSIMMu` and `voiceUSBTransition` remain for other workflows; retaining those locks is not retaining a Go cellular owner.

Offline contract and fake/race evidence does not certify physical call audio, modem re-enumeration, carrier behavior or a Go-free application. Current structural exit remains blocked by six categories and 257 production Go files.

## Iteration479 reviewed lifecycle boundary

The complete module call-mode lifecycle lives in `call_mode_lifecycle.rs` and `call_mode_state.rs`. Public status, enable, restore, retry and download use that one native owner. It owns download/progress/verification, backup capture and live restore matching, authorization disclosure, typed USB/voice actions, identity-checked reconnect and initialization, runtime deployment and actual cleanup. Go has no parallel status cache, generation, operation or worker. Other Go call/SIM/USB and IMS session execution remains a separate migration boundary; this is not whole-app Go exit or device acceptance.

The four exact private contracts are GET `status` and POST `readiness`, `invalidate`, `shutdown` under `/api/internal/call-mode/lifecycle/`. Private listener/token admission applies to all four. POST rejects queries/unknown fields and admits at most 1024 bytes in 3 seconds, naming the current `producer_id`. GET is the standalone Go fallback alias; Rust public status never calls Go. Readiness requires both `ready` and `operation_running` and fresh native USB/voice/ADB/runtime/boot checks. Invalidate only advances native readiness. Shutdown captures the admitted producer, holds the cleanup gate, cancels that producer, drains actual operations/read/release tasks, rechecks identity and confirms native stop before returning `ready:true`. Failed/lost observations never replay physical work.

Admission has a retained read pin before task spawn. Every acquired reservation, including early rejection and refresh, remains owned until idempotent release confirms actual settlement; only then can the operation end. Draining enables its Notify waiter before checking the predicate. Producer replacement waits for the old lifecycle and shutdown gate before radio retirement; stale shutdown cannot stop the replacement. Radio release tombstones prove actual release or completed retirement drain, not assumed success.

Native rebind checks deadline/epoch cancellation before candidate open, during identity probing and before installation. `AT+CGSN` drains its terminal response. Unknown identity candidates receive no initialization/configuration. Once identity matches, the fixed initialization queue restores required initialization and confirms the original native receive preference before binding commit. Cancellation closes only that worker's uninstalled resource, never a newer borrowed session.

Private `/api/internal/modem/open` atomically acknowledges the installed positive `serial_session` inside the actor. Go records that immutable identity. Private `/api/internal/modem/close` requires `{serial_session>0}` and returns `{closed:true,serial_session:<requested>}`. Matching close actually closes that stream; stale/already-closed close is idempotent and preserves the new stream, initialization and pending reads. Go rejects missing/zero/mismatched identity, never sends an unfenced `{}` cleanup and never retries a lost ACK. Native internal shutdown has its separate actor Close command. This fence covers physical close; it does not claim all generic modem clients have retired.

Retired Go source scopes: `call_mode_backup.go`, `call_mode_backup_store.go`, `call_mode_download_client.go`, `call_mode_status_projection.go`, `call_mode_download_adapter.go`, `call_mode_retry_adapter.go`, `call_mode_status_adapter.go`. Retired POST registrations under `/api/internal/call-mode/`: `status/observe`, `enable/start`, `restore/start`, `retry/observe`, `retry/start`, `runtime/download/admit`, `runtime/download/progress`. Their handlers, callers and main lifecycle fields were reviewed before deletion. `call_mode.go` retains read-only types/path helpers, `call_mode_adb_client.go` retains ordinary stop, and `call_mode_lifecycle_client.go` retains native compatibility clients. Legacy Rust projection/backup/runtime private endpoints remain callable; they no longer have the deleted Go lifecycle consumer.

The reviewed function boundaries are:

| Retired Go responsibility | Current native authority / remaining caller |
| --- | --- |
| `currentCallModeStatus`, `inspectCallMode`, `inspectCallModeUnlocked`, `callModeSnapshot`, `setCallModeStatus`, status projection clients and status-observe adapter | Native revision-fenced state, reservation-bound inspection and retained refresh; the Go status method is only a private-alias proxy |
| `beginCallModeOperation`, `endCallModeOperation`, operation file-lock workers and duplicate voice generation/readiness | Native state generation, operation/read guards, retained admission/cleanup owners and boot-bound runtime authority |
| `runCallModeEnable`, `finishCallModeAfterUSB`, voice/USB read-write and ADB authorization sequencing, enable-start adapter | Native enable worker, explicit confirmation/disclosure, fixed typed radio plans, identity-checked reconnect and initialization |
| `runCallModeRestore`, `finishCallModeRestore`, module/backup matching and backup-store clients, restore-start adapter | Native authoritative no-follow backup service and reservation-bound live identity/composition matching before fenced writes |
| `runCallModeDownload`, `prepareDownloadedRuntime`, runtime verify/download clients and download admit/progress adapters | Native blocking download owner with process lock, pinned verification/progress and the same lifecycle's deployment worker |
| Retry-observe/retry-start adapters and Go retry admission | Native retry/force-refresh and matching-operation snapshot |
| `prepareModuleVoiceRuntime`, `selfTestModuleVoiceRoute`, `moduleVoiceRuntimePrepared` and Go worker polling at shutdown | Native runtime preparation plus actual shutdown drain; Go readiness/reset/shutdown methods query or notify native authority once |

Public and private route names are retained where their behavior is still supported. Function names above identify retired Go responsibilities; compatibility client methods with matching public names do not regain state or physical sequencing. Ordinary stop/hangup policy, transport synchronization, unsupported-platform behavior, surrounding call/SIM/USB callers and Go process startup remain outside this retirement claim.

This review adds no hardware/SIM/SMS/call exercise, no new score and no extra handler credit for HEAD aliases. Full Go structural exit remains blocked by six categories and 257 production Go files at the latest census.

## Ownership and authentication

Rust is the only writer of `module-flight-mode.json`, retaining `enabled`, `restore_cfun`, and `user_configured`. Missing preferences mean default RF-off; malformed/unreadable preferences fail closed. Atomic private file replacement and the existing user opt-out semantics are preserved. A single tracked Rust worker applies the default policy every 3 seconds, without overlapping operations.

All private endpoints below use POST, strict JSON and `X-Switch-Internal-Token`. Every request carries `producer_id`, equal to the current Go child identity supplied by `SWITCH_CALL_PRODUCER_ID`. The Rust supervisor fences replacement children; a previous producer cannot rebind, restore, release or overwrite its successor. Go never supplies a filesystem path or a guessed serial generation.

The existing public GET/POST `/api/module/flight-mode` become Rust-owned. POST retains `{enabled:bool,user_initiated:bool}` and the existing status/error schema. Go's old public registrations may only forward compatibility requests; they must not retain a second policy or file writer.

## Common wire rules

Tokens are generated by Go before submitting the request: nonempty, at most 128 bytes, ASCII alphanumeric, `-` or `_`. Sequences are unsigned nonzero 64-bit integers, monotonic independently for configuration, each reservation purpose, and VoWiFi intents. Overflow permanently fails admission. The same sequence and token identify the same logical operation, never a new physical attempt.

Acquire/prepare and cleanup are idempotent for the exact identity. A release/restore which arrives before its acquire/prepare advances that class's released high-water mark; the late acquisition cannot resurrect it. Cleanup never releases another active token. Unknown transport/ACK outcomes retain the original token/sequence for cleanup; callers do not manufacture a replacement token and replay a physical action.

Errors use `code`, `error`, `retryable`, with `submitted`/`uncertain` where execution may have started. Go validates required fields and matching token/sequence/session echoes. Absence of confirmation is not success.

## Private endpoints

All examples omit the required `producer_id` only for brevity.

| Path under `/api/internal/module-radio/` | Request | Success response |
| --- | --- | --- |
| `configure` | `{config_seq,transport:"usb"\|"serial"\|"none",session_id}` | `{configured:true,config_seq}` |
| `query` | `{}` | `{preferences:{enabled,restore_cfun,user_configured},suppressed,radio_owned,status,error_code}` |
| `bootstrap` | `{}` | Existing public flight-mode status |
| `refresh` | `{}` | Existing public flight-mode status |
| `reservation` acquire | `{action:"acquire",purpose,token,sequence}` | `{token,sequence,released:false}` |
| `reservation` release | `{action:"release",purpose,token,sequence}` | `{token,sequence,released:true}` |
| `vowifi` | `{action:"prepare"\|"enter"\|"observe"\|"restore",token,sequence}` | `{token,sequence,active,restored,cfun_mode,verified}` |
| `action` set | `{purpose,token,sequence,operation_id,kind:"set_mode",target:0..4}` | Physical outcome below |
| `action` reboot | `{purpose,token,sequence,operation_id,kind:"reboot"}` | Physical outcome below |
| `action` profile refresh | `{purpose,token,sequence,operation_id,kind:"refresh_profile"}` | Physical outcome below |
| `action` diagnostic | `{purpose:"diagnostic",token,sequence,operation_id,kind:"diagnostic",command,timeout_ms}` | Physical outcome including bounded original response |

`purpose` is exactly `esim`, `call`, `call_mode`, `reboot`, or `diagnostic`. An action requires the matching currently admitted purpose/token/sequence; a raw caller cannot present a preference revision as authority. `refresh_profile` requires eSIM authority; reboot requires reboot/call-mode or permitted diagnostic authority. Diagnostic `set_mode` preserves the old console policy: mode 4 is allowed, cellular-enabling modes require the explicit cellular opt-out and an observed permissive state. Diagnostic changes do not become preference writes.

`configure.session_id` is required and nonempty only for USB; serial/none use an empty string. Rust validates the current USB handle session. For serial, the actor captures its real `serial_session` inside the actor; SMS/SIM generation is not a descriptor fence. Identical configuration sequence/body is idempotent; an older sequence or a changed body at the same sequence is rejected. Configuration never silently migrates an active intent to another device. No binding uses `none` and returns an unavailable observation without hardware access.

`query` is a cached policy projection suitable before serial initialization; it does not execute AT or depend on the Go public listener becoming ready. Unreadable preferences return safe `suppressed:true` with an error code, not a fabricated successful preference. `bootstrap` is an actual initial policy barrier, invoked after configuring the opened transport and before eSIM startup work. Failure is visible and leaves RF telemetry suppressed. `refresh` performs a bounded actual observation; no arbitrary AT text is accepted.

Every `action` request also requires a Go-preallocated `operation_id` (same token alphabet/limit). The active reservation retains up to 64 action identities with their original request and result: same ID/payload returns the original outcome; changed payload conflicts; capacity exhaustion rejects before submission. Unknown outcomes do not permit a new ID to replay the action without a read-only reconciliation. Go never automatically retries physical actions and preserves the original operation ID in an unconfirmed error.

Every physical outcome field is present:

```json
{
  "complete": false,
  "submitted": false,
  "cfun": null,
  "verified": false,
  "uncertain": false,
  "error_code": "",
  "cereg": null,
  "creg": null,
  "signal": null,
  "restore_confirmed": false,
  "response": ""
}
```

Numeric optional observations are integers or explicit null. A submitted write followed by transport loss cannot be reported as verified. A non-complete/uncertain outcome remains failure even if a command was submitted. Reboot acceptance is a terminal command observation, not proof that a replacement module has booted successfully.

## Reservations, intents and physical transactions

A business reservation prevents RF transitions while its Go caller is active. Admission is owned atomically by Rust, not by a Go busy-status snapshot. Conflicts return busy without occupying an IPC connection indefinitely. Call/eSIM/call-mode/reboot/diagnostic callers acquire before real work and release only after that work settles. HTTP cancellation is not a claim that hardware work ended.

A VoWiFi intent is separate from the short physical transaction. `prepare` reserves RF ownership and the native USB receive inhibit, but keeps AT/SIM available for identity and AKA. `enter` captures the original CFUN on the fixed binding and confirms mode 4. Repeated enter for the same live intent does not replace its original recovery target. Reconnect carries the same intent; a newer session cannot be affected by an old restore. `observe` uses the Rust-owned observation state/refresh policy. `restore` consults the latest persisted preference, including fail-closed/default RF-off precedence, and releases ownership/inhibit only after actual confirmation. Restore-before-prepare performs no CFUN work and fences that late prepare.

Go eSIM switching first stops module VoWiFi and confirms its actual cleanup/restoration, then acquires the eSIM reservation before card I/O. If a new VoWiFi startup wins the intervening admission race, eSIM fails busy with zero card mutation. There is no nested parent reservation and no Go mutex held across a reverse Rust-to-Go call.

Physical transactions use one USB owner transaction permit for the complete fixed-session plan (an existing SIM/voice lease causes immediate busy), or a serial actor-owned typed completion with deferred polling. Long-lived reservations/intents do not hold this physical lease. All steps, follow-up observations and rollback retain the same transport session. Old-session recovery rejects rather than auto-opening or selecting a replacement handle. The serial actor also observes call URCs; physical CLCC preflight preserves existing live-call semantics (2/3/4/5 live; 0 live only with a nonempty number; 6 ignored). History persistence is not a source of radio authority.

## CFUN bypasses and lifecycle

Manual AT, including single `AT+CFUN=N`, `AT+CFUN=1,1` and legal compound forms, uses the Service-only typed Diagnostic action under diagnostic authority. The original bounded command is executed intact on the fixed binding; it is not split or rewritten into a different sequence. Rust independently enforces the existing 4096-byte, single-line, UTF-8/control-character and quote-aware policy checks, including both backslash-quote interpretations. `timeout_ms` is nonzero and at most 20000. `AT+CFUN?` and `AT+CFUN=?` remain read/capability queries. Policy-forbidden operations are rejected before submission; successful diagnostic execution invalidates cached RF observations and returns the actual bounded response. The shared Rust `module_radio_transport::is_radio_write` classifier handles case, spacing, quoted arguments, Hayes prefixes and separators at both generic raw USB and serial submission boundaries. Those generic routes reject RF writes; only the internal Service-authorized physical action can execute them. Go parsing does not replace Rust authorization.

Production call-mode reboot, public reboot and eSIM CFUN0/restore use typed actions. Legacy backend setters that have no production authority cannot remain hidden raw writers. QMI/MBIM implementations are not asserted migrated by this AT policy contract.

Startup installs/configures policy before eSIM work; serial RF telemetry remains suppressed until its Rust projection allows it. USB state collection uses the Rust suppression decision. During shutdown, new bootstrap/default transitions and new business acquisitions stop, but the current producer's existing-token observe/restore/release remain admitted. Native workers and hardware operations are joined before owner close. The existing bounded Go shutdown may return an explicit unconfirmed result; it must not mark outstanding intent/transport work completed merely because its context expired.

Bootstrap must acknowledge actual observation/application. A missing binding, active business reservation/VoWiFi intent, or unresolved physical outcome returns an error instead of a successful skipped projection. Go checks the required status fields and confirmed availability before opening dependent eSIM work. A later successful barrier on the same binding can install a missing eSIM manager, after releasing the bootstrap lock and without automatically issuing card I/O. Serial configure recaptures the actor's real session; an empty Go serial session string is never treated as a permanent binding cache key.

Original call URCs only enqueue one bounded, coalesced desired reservation revision. A joinable worker performs RPC outside history locks; it releases a late acquisition using its original identity. Number-only CLIP changes preserve an existing active/inactive intent and cannot create or revive a call. Outgoing admission and pending-dial publication share the coordinator lock, so a delayed idle observation cannot release a newly acquired reservation before ATD. Shutdown first confirms source exit, then waits for this worker's actual release. A retained pointer whose release is unconfirmed grants no authority to a new dial.

The native Service publishes cellular suppression directly to both physical owners. USB commands and serial queued/typed queries recheck it at actual I/O admission; a stale Go metadata hint cannot allow a cellular probe. Go's existing status/metadata collection mirrors the native projection, including the transition back to permitted telemetry, without introducing a second policy timer.

## Required offline acceptance

- Existing public DTO, preference/default/manual opt-out, RF-off no-probe and restore-precedence vectors.
- Preference write failure, failed verification, rollback failure and file permissions/atomic replacement.
- Single-worker fake-clock scheduling, startup barrier and real shutdown join.
- Every required wire field, token/sequence echo, stale child/session and release-before-acquire ordering.
- ACK loss keeps original cleanup identity; no physical write replay or restoration on a replacement device.
- eSIM stop/acquire competition, call/call-mode reservations, incoming-call actor checks, and AKA usable during VoWiFi ownership.
- Generic USB/serial CFUN bypass rejection alongside supported typed manual translation and capability queries.
- Go/Swift public behavior, cancellation before admission and cancellation after submission.

These are implementation gates, not evidence of real modem, carrier or SIM validation.

The Iteration096 public-method acceptance is limited to the existing Swift GET/POST calls. The old Go GET registration also accepted HEAD implicitly; the new native dispatch does not yet normalize HEAD and that request falls through to Go compatibility. HEAD/405 equivalence remains an explicit P0 public-method acceptance gap, to be handled consistently before release; this contract does not claim it has passed.

## Request budgets

Each Rust Service job waits at most 30 seconds for its gate, then shares a 55-second physical budget across observation, writes, verification and rollback. Go private calls use 95 seconds; reservation release uses an independent 65-second budget and retains its token if unconfirmed. Swift GET/POST `/api/module/flight-mode` use at least 100 seconds. These are response budgets, not cancellation of submitted I/O or filesystem commits; actual workers remain tracked. Manual diagnostics retain their physical maximum of 20 seconds.

## Iteration 666 public mutation caller receipts

`public_mutation_receipt.rs` is the permanent Rust caller ledger for raw mutating
AT, flight mode, USB network mode, module ME phonebook notes, public call-mode
enable/restore and public audio start/stop. Each write requires a strict
`client_request_id` (8–128 ASCII letters, digits, `-_.:`). The exact route and
original request bytes form the fingerprint. An atomic fsynced unknown receipt
is installed before dispatch; another claim with the same ID returns that
receipt, and a different payload conflicts. Terminal identities are never
removed to make room; saturation rejects new requests. Empty/null/array/broken
archives, missing archives after enrollment, and missing/corrupt presence
markers fail closed across a new Store instance. Dispatch failures with no
confirmed response remain unknown and cannot automatically replay.

Raw AT remains supported. Only the finite `proven_read_only_at` allowlist may
execute without caller identity; an unrecognized command is a mutation. Existing
RF policy and module VoWiFi channel exclusion still run in the actor. Radio
mutation admission checks the captured producer, epoch, configuration, transport
session and USB card identity generation under the same lock that admits the
job. ME note ICCID and slot/content are included in the exact request fingerprint.

Call-mode outer receipts retain the original lifecycle admission response and
operation generation. Existing backup capture, hash verification, physical
restore journal, release receipts and real cleanup obligations remain
unchanged. An old restore caller cannot begin a later lifecycle. Audio start's
caller ID is its public route generation; stop additionally requires that exact
`audio_generation` and the original module binding. Public audio admission is
serialized through readiness and route dispatch; a replay never selects the
currently running successor route.

Swift's `PublicRadioMutationCheckpoint` persists and reads back the exact ID and
request bytes before upload. Transport/decode loss preserves them across new
APIClient/GUI instances, payload drift is fenced, and only a matching completed
Rust receipt clears them. Corrupt and persistence-failed checkpoints remain
sticky; a failed clear restores the original identity. A confirmed stop may
settle only its exact preceding audio-start checkpoint. These are software
admission guarantees; no hardware, operator, acoustic or long-duration
acceptance is inferred from fixture tests.
