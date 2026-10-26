# Device SMS v2 production contract

Status: implementation contract, not a test or rollout result. Baseline: archive-only commit 98dd71149af7eaf82578081a6c238bdbfe2ed3c9. No hardware, carrier or endurance acceptance is implied.

## Ownership

A owns device_send_store.rs, authority_v2.rs and a new authority_v2/production.rs plus their tests; device_sms_send.rs and tests; SMS-only startup/dispatch edits in main.rs.
B owns proletarius-store raw.rs/device_sms_seal.rs and tests; authority_v2/family.rs/seal_adapter.rs and tests; daemon_lifetime.rs production enrollment.
C owns DeviceSMSSendStore.swift, DeviceSMSSendLive.swift, a durable issuance journal and Swift tests. Coordinate against the archive-only baseline, not old local source.

Migration is startup-only. A adds Store::open_with_authority(root, RootProof), called with lifetime.root_proof() after the actual lifetime lease and before publishing any Store/Owner to HTTP or background services. Its private StartupMigration token holds that same RootProof; validate(proof) uses same_authority. Only this constructor creates the token, while the newly constructed Owner has never been published or admitted a job. There is no live migration or HTTP migration endpoint. Existing submitting v1 entries remain non-claimable and recover to unknown; prepared legacy identities never gain a new claim after migration.

## B adapter API

- Adapter::open_or_migrate(root: &Path, proof: RootProof, startup: StartupMigration) -> Result<Adapter, Error>.
- Adapter::transact<T>(&self, f: impl FnOnce(&[u8], &[u8]) -> Result<(Option<Vec<u8>>, T), CodecError>) -> Result<T, Error>.
- Callback arguments are exact current v2 bytes and exact frozen v1 bytes under the ORIGINAL writer lock, already checked for provenance/digest. None means read-only, with no revision change. Some is a proposed complete v2 snapshot; schema and its issued high water must be revalidated under the same lock before committing.
- Error::Rejected(CodecError) identifies a business refusal; invalid current schema, I/O, missing provenance and uncertain commits latch the adapter unhealthy. Adapter::restart_required() remains available.
- AuthoritySnapshot::issued_high_water() -> u64 is supplied by A. Existing encode_checked/decode_snapshot_bytes remain the strict v2 codec boundaries.
- B discovers identity from coherent dual witnesses/journal or a valid exact transaction WAL. Only a complete valid virgin v1 family permits new random authority/generation. No caller-supplied or HTTP-supplied identity initializes a store.
- B may add one fixed bounded device-send-ledger.v2-transaction.json WAL for old/next witness plus exact target bytes. Journal, frozen, prepared and active are one original-lock protocol. Every interrupted prefix must either recover its exact proposed bytes under a new real lifetime or fail closed. The failed current lifetime never clears sticky health.
- Full v1 capacity must migrate without deleting any record. Current original-path v2 and witnesses must reject the supported v1 decoder before any old dispatch admission.

## HTTP wire: reuse existing Rust-owned routes

No new route or ownership-table row is needed.

1. GET /api/sms/send-intents?authority=2 is the read-only authority status. Mixing authority with id/cursor is invalid.
2. POST /api/sms/send-intents with the nested reserve DTO below is the only issuer. It performs no capture/claim/physical I/O.
3. POST /api/sms/send-intents with a flat send DTO binds an existing reservation to the current selected route (prepare).
4. POST /api/sms/send with the same flat send DTO performs the sole claim, or returns the original outcome. An accepted unarchived outcome enters only the archive recovery from the first batch.
5. GET /api/sms/send-intents?client_request_id=... and list/cursor remain read-only and never issue or rebind an identity.

Status JSON:
```json
{"identity_version":2,"family":"device_sms","authority_id":"0123456789abcdef0123456789abcdef","epoch":"1","issued_high_water":"0","next_sequence":"1","next_client_request_id":"m2:ds:0123456789abcdef0123456789abcdef:0000000000000001:0000000000000001"}
```
At sequence exhaustion, next_sequence and next_client_request_id are null; there is no wrap or epoch rotation.

Reserve JSON (canonical counters are decimal STRINGS, nonce/authority exactly 32 lowercase hex):
```json
{"identity_version":2,"family":"device_sms","authority_id":"0123456789abcdef0123456789abcdef","epoch":"1","sequence":"1","client_request_id":"m2:ds:0123456789abcdef0123456789abcdef:0000000000000001:0000000000000001","intent_nonce":"abcdef0123456789abcdef0123456789ab","purpose":"device_sms_send","device":"reader","payload":{"phone":"+12025550100","message":"example","expected_subscription_id":""}}
```
Only reader/dji are allowed; explicit device is mandatory for v2. Original normalized phone, exact message and expected subscription are bound. The reserve reply is the normal projected Record, state reserved, can_submit false, identity_version 2. Repeating the exact candidate/nonce/payload returns its original record; a different nonce/payload at that position conflicts and never silently advances.

Flat send DTO:
```json
{"identity_version":2,"client_request_id":"m2:ds:0123456789abcdef0123456789abcdef:0000000000000001:0000000000000001","phone":"+12025550100","message":"example","device":"reader","expected_subscription_id":""}
```
identity_version omitted or 1 means legacy. Every projected Record includes identity_version (1 or 2); v2 also supports state reserved in addition to prepared/unknown/accepted. Submitting still projects unknown. Binding and attempt remain private. Existing timestamps and archive_confirmed retain their meaning. Reserved cannot submit until prepare captures and durably saves an exact binding. Preparing an already bound reservation never rebinds it.

## Lookup, errors and retirement

Legacy lookup is checked against the complete frozen v1 index before v2 parsing; IDs that resemble v2 stay legacy. Existing legacy IDs remain queryable with original payload. Unknown unversioned IDs after migration get 409 mutation_identity_upgrade_required. An old pending legacy intent is never renumbered automatically.

Current-authority issued seq absent from live/cache gets 410 mutation_identity_retired. Future seq gets 409 mutation_identity_not_issued. Foreign authority/epoch gets 409 mutation_authority_mismatch. Candidate collision or nonce/payload drift gets 409 send_intent_conflict. Invalid fields get 400 invalid_request. Capacity gets 409 ledger_full; exhaustion gets 409 mutation_sequence_exhausted. Missing/corrupt storage gets 503 sms_store_unavailable. Error responses include retryable:false and request_dispatched:false (only the current HTTP request; no statement about historical delivery).

Fixed production quotas retain current 32 live slots, 16 per reader/dji lane, 32 recent terminal cache records, 4 MiB terminal cache, 32 MiB active envelope and original bounded 32 MiB frozen v1. Unknown, submitting, and accepted-unarchived remain live; none can be evicted by time, quota or GUI dismissal. Only never-dispatched cancellation or accepted+archive_confirmed can retire into bounded cache. Payload, binding, attempt, segments and timestamps are preserved until safe cache eviction. High water never decreases. Legacy archive-recovery overlay may change only accepted false->true (and interrupted submitting->unknown), never grant a second claim.

Archive deletion must remain possible for imported send_request_id rows absent from the legacy/live indices and for retired v2 identities: neither can be recreated or claimed by reconciliation. Exact accepted-unarchived rows first confirm the original durable archive receipt under the archive lock. Corrupt ledger/provenance fails closed. archive_confirmed means historical archive acknowledgement, not that the row remains visible.

## C durable issuance

Before reserve, save the exact reserve request bytes, flat send bytes, candidate identity, nonce and version to a bounded local journal, with cross-process flock, restrictive permissions, atomic replacement, file fsync and directory fsync. Read back the identical checkpoint before sending. Reserve ACK loss/restart replays those exact bytes and same candidate. After a confirmed reserve, persist its state before prepare/claim. Neither transport failure, 404, 410, corruption, mismatch nor capacity failure creates another identity. A user-requested new message is a separate explicit action.

Read old pending identities without rewriting them. reserved checkpoints may resume reserve/prepare using their same payload; prepared checkpoints obey original binding; accepted-unarchived uses explicit archive-only recovery; unknown and retired stay unresolved and cannot auto-send. Only matching confirmed terminal outcome removes the issuance checkpoint. GUI dismiss is presentation only and cannot retire a ledger slot.

## Required GitHub validation

Compile Rust production and codec feature; Swift isolated fixture suite; old decoder rejection; exact reserve replay and two-client CAS; claim-before-dispatch failure; full v1 migration; every fixed WAL/migration failure prefix; archive ACK loss/delete/reconcile races; tiny-quota long terminal loops with reopening and old-id replay after eviction; byte budgets and unknown-slot saturation. No local tests/builds or real SMS are authorized by this contract.

## Integration amendments (A/B/C coordination)

Flat v2 send DTOs also require intent_nonce with the same 32 lowercase hex value as reserve. Every v2 Record, including accepted and cancelled, includes intent_nonce and the original expected_subscription_id. The latter remains empty when the original user did not select a subscription, even after subscription_id contains the actual captured route. This prevents the losing nonce of a same-sequence CAS conflict from preparing, claiming or clearing its checkpoint using the winner's identical business payload.

POST /api/sms/send-intents?cancel=2 accepts the original full nested reserve DTO. Only an unbound reserved slot can cancel; bound prepared/submitting/unknown/accepted refuse. The confirmed result is state cancelled, can_submit false, with original nonce/payload. It burns the sequence and uses the existing bounded not_dispatched cache. A previously retired sequence still returns 410. A GUI without its journal may explicitly cancel a reserved entry using the original public nonce/payload fields; it cannot reconstruct a new submit intent. CAS losers retain their conflicting checkpoints; a separately confirmed new-message action may use another bounded journal slot.

No new route registration is introduced: GET/POST /api/sms/send-intents and POST /api/sms/send remain registered in main.rs, ownership.rs and public_http. Only the query/DTO/schema behavior changes. Store conflict maps to the existing public sms_intent_conflict spelling. GET authority=2 is an exact query; mixed/unknown fields reject. Existing public/internal endpoint review must include these schema changes.

The codec retains immutable reserve metadata and adds an optional bounded execution object containing binding, actual subscription, state, original attempt, segments, archive flag and update time. Payload bytes are not duplicated. execution=None is the sole cancelable unbound state. Only accepted archived execution can enter the terminal cache. Store startup registers a weak reference to its real adapter so archive deletion can obtain that same authority by root without a second enrollment or a health reset. Missing/non-issued/foreign/retired identities permit deletion without creating any ledger evidence; corrupted provenance remains an error.
