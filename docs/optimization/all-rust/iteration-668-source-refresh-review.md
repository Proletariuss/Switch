# Iteration 668：版本化来源与范围审阅

本轮只更新活跃元数据及来源检查器，不改Rust/Swift生产源，不运行Cargo、硬件或外部业务。i666 snapshot/QMI/review和既有冻结entry文件原字节保留；精确前置备份及执行脚本位于build/reports。

当前快照509文件，前置i666为503；精确变更23文件，新增6文件。snapshot SHA-256 `d8729b3084fdee7495344712dd261b65241a6db75b97b77507428c38a1c6a113`。累计11个不同contract生产scope已逐项人工复核；新receipt事务修复后受控再生census，初稿与本修订执行回执分别保留；仍304/304 Rust、0Go、125 Swift HTTP、470scopes、57HEAD。

## 逐文件语义审阅

- `app/Sources/BackendProcess.swift`：Default launcher selects serial/AT, not QMI. Explicit backend=qmi remains separate. No direct Swift QMI private RPC was found; QMI physical behavior is unresolved.
- `app/Sources/DashboardStore.swift`：Preserve module VoWiFi transport preference when reflecting status; effective shared proxy selection is backend-owned.
- `app/Sources/ESIMModels.swift`：Decode bounded startup diagnostics and attempt/retry/transport projections without interpreting readiness as delivered SMS or calls.
- `app/Sources/ModuleVoWiFiStore.swift`：Module start carries frozen user proxy/FQDN/port intent and backend receipt observation. Local status and retry displays do not manufacture carrier readiness.
- `app/Tests/BackendReadiness/main.swift`：Readiness fixture expects the actual default AT launcher; does not establish QMI handshake.
- `app/Tests/VoWiFiReadiness/OperationTests.swift`：Exercise retained operation identity and module connection settings; simulations do not claim external business acceptance.
- `rust/crates/proletarius-vowifi/src/auth.rs`：Final IKE_AUTH drops early ESP on the shared NAT-T channel under the original bounded deadline; it neither consumes unauthenticated ESP nor extends the deadline.
- `rust/crates/proletarius-vowifi/src/child_data.rs`：IPv6 ESP protocol 50 is terminal for CHILD selectors. Address/protocol/full-port selectors remain mandatory; unsupported extension headers/fragments remain rejected. Failure diagnostics are fixed enums. i668 nested CHILD/IMS ESP composed test additionally preserves direction policy and both replay layers.
- `rust/crates/proletarius-vowifi/src/ims_profile.rs`：EAP-direct material policy derives from the resolved carrier template InitialAuthorization, replacing the daemon-side carrier/PLMN whitelist.
- `rust/crates/proletarius-vowifi/src/lib.rs`：Export bounded PolicyRejectReason for production startup diagnostics; no new HTTP route or external endpoint.
- `rust/crates/proletarius-vowifi/src/netstack_device.rs`：Add an independently assembled IPv6 pseudo-header/TCP checksum test with a damaged-packet negative assertion; production packet construction unchanged.
- `rust/crates/proletarius-vowifi/src/sa_init.rs`：SOCKS domain mode uses explicit ForceNat evidence and deterministic port-zero NAT-D hashes; never resolves the ePDG locally or adds direct fallback. Transparent socket retains exact numeric tuple.
- `rust/crates/proletarius-vowifi/src/startup_loopback.rs`：Loopback checks exact SOCKS NAT-D insertion and subsequent protected exchange; test-only protocol evidence does not establish live other-carrier registration.
- `rust/crates/proletarius-vowifi/src/transport.rs`：Expose Exact versus ForceNat evidence; pinned NAT-T delegates without disclosing or guessing proxy-owned ePDG numeric addresses.
- `rust/crates/proletariusd/src/native_vowifi_session.rs`：Retain pre-cleanup network-ended fact for bounded diagnostics, derive EAP-direct policy from template, and project frozen non-secret transport/proxy-source facts for both reader/module.
- `rust/crates/proletariusd/src/native_vowifi_session/configuration/tests.rs`：Same production carrier resolver matrix for CTExcel/VDF/3UK/giffgaff/CMLink/Loop/SIMWorld; model/operator display names do not override registry selector evidence.
- `rust/crates/proletariusd/src/native_vowifi_session/module.rs`：Retain one public operation and frozen connection across bounded initial retry; distinguish cleanup failures and preserve join/claim release. Explicit stopped cleanup clears obsolete startup errors only after owner retirement. Final same-lock status/clock projection reviewed separately with the parent test evidence.
- `rust/crates/proletariusd/src/native_vowifi_session/module/request.rs`：Decode module FQDN/proxy/port via shared normalization; bare epdg_ip_address is rejected to retain proxy DNS/certificate identity. Disable uses automatic empty connection.
- `rust/crates/proletariusd/src/native_vowifi_session/module/tests.rs`：Production module fixture covers operation/request/stop/retry outcomes; capture uses the real canonical request_fingerprint. Parent owns actual test execution.
- `rust/crates/proletariusd/src/native_vowifi_session/operation.rs`：Retained operation projection supports bounded retry identity; exact owner/successor receipt fencing remains required.
- `rust/crates/proletariusd/src/native_vowifi_session/operation/tests.rs`：Exercise receipt identity and retained operation transitions; no replay of unknown physical submission is authorized.
- `rust/crates/proletariusd/src/native_vowifi_session/startup_diagnostics.rs`：Map network/task/IKE/policy/io failures to bounded enums and counters only; no SIM, endpoint, packet, key or SIP contents emitted.
- `rust/crates/proletariusd/src/native_vowifi_session/startup_diagnostics/tests.rs`：Test packet-transform and IKE error projection, bounded fields and absence of raw packet/error payload.
- `rust/crates/proletariusd/src/network_recovery.rs`：Terminal successor records make stale-worker updates durable no-ops; applied=false stops stale checking/reboot/readback and preserves unknown without replay.
- `rust/crates/proletariusd/src/network_recovery/tests.rs`：Wait for exact runtime inactive barrier and verify durable bytes remain unchanged after stale reboot release.
- `scripts/check-acceptance-ledger.py`：Advance active source/QMI identity to i668 while pinning immutable i666 predecessor; default AT semantic boundary explicit, no functional PASS/denominator freeze.
- `scripts/tests/test_acceptance_ledger.py`：Add successor/predecessor tamper checks and preserve cross-group pins, exact source-set and product/device anti-promotion guards.

## QMI与验收边界

BackendProcess的默认参数实际为backend=at，当前trace用swift_default_at_launch记录；不再声称默认启动QMI。59个anchor中只替换Swift producer anchor，8个QMI源文件和非Swift fragment保留，QMI物理能力与消费者可达性UNRESOLVED_NOT_PASS。i667指定AT注册不等于QMI或完整运营商业务通过。

P0–P6未全部闭合，P5正式16叶子，83.3/10/12没有冻结依据。0新增功能PASS、0新需求/计分实例，产品分母未冻结；六组原软件排除项不提升PASS。B01–B12最后的同源全门/冷构建、lifecycle、handoff、P6、closure、三轮review/attestation仍待实跑。

入口映射SHA-256 `e9d690b46736aefb9d05573289a0afbdc5225097bb7c870add7a3e5833ceb90b`；runtime map `12536ce1827274af6b65d2b6151164b999461781817383878201ddb78f1c355b`；QMI trace `5b50d0467055dca0ff7fd0201f6d7a1b3111370e865f9b28a9e69e377c17f787`。

## 证据与限制

contract逐项原差异：`build/reports/iteration-668-ten-scope-manual-review.diff`；独立VoWiFi审阅和module最终测试由主线产生，不能在本来源刷新中伪写为完整门禁通过。运行ledger checker与负向单测的真实结果另在build/reports保留；本文件仅来源/范围审阅。

## 来源pin显式化纠正

首次ledger检查发现auth.rs两组pin原先只保存sha256，检查器把它隐式视为冻结源身份；刷新时现已显式保留原67fa9b85…为frozen_cut_sha256，并如实保持matches_frozen_cut=false。未改源码、snapshot、QMI、需求或验收状态。首次失败报告保留，后续新报告另名创建。

## i668 r2受控草案修订

前一未发布草案完整字节已保存于build/reports/iteration-668-pre-r2-*。i666前代文件保持原字节。新增reader/service.rs及service/tests.rs审阅范围，精确变更集合现为23个原快照文件，新增仍为6、总509。额外人工审阅reader service.rs，并复审module.rs；本轮累计11个不同生产scope，census仍470 scopes。

- `rust/crates/proletariusd/src/native_vowifi_session/module.rs`：Current operation/status sample projects one retained record under one RecordState guard; result uses that same live sample. After frozen receipt resolution, companion status is included only when operation_id, generation and requested_enable match current; a different historical operation omits status. Receipt lookup/observation now acquires service state before selecting/projecting current record and mutating RequestLedger, so a successor cannot be inserted between projection A and observation. Preserve service→record lock order and unknown/cleanup fences.
- `rust/crates/proletariusd/src/native_vowifi_session/module/tests.rs`：Regression keeps cleanup/status pair coherent and prevents a newer durable receipt from being frozen superseded by stale projection; fixtures use actual canonical request_fingerprint.
- `rust/crates/proletariusd/src/native_vowifi_session/service.rs`：Reader receipt lookup/observation shares the admission State transaction with current selection and RequestLedger observation. collect_cleanup_result drops driver.current before lifecycle take_result, then rechecks exact attempt key and handle generation after reacquiring it, closing the identified State→driver→lifecycle→State ring. Concurrently retained same-attempt results survive a second collector error. Global RequestLedger.observe semantics unchanged.
- `rust/crates/proletariusd/src/native_vowifi_session/service/tests.rs`：Deterministic reader successor-receipt and real joined-result collector fixtures exercise observer/shutdown overlap without holding driver.current during lifecycle harvest; idempotence, competing collector result and attempt drift remain fail closed. The joined-cleanup fixture intentionally acquires one lease (pre_calls=1) and dispatches zero APDU; this is not real card or operator execution.

新snapshot SHA-256 `d8729b3084fdee7495344712dd261b65241a6db75b97b77507428c38a1c6a113`；前一草案`e15340108fffee46cf6824c115fb596ed8d192fe7001712266e626f0cff02ab4`仅作修订前证据。最终同源门禁/冷App/lifecycle/handoff/P6/三轮review尚待完成，无新增PASS或分母冻结。
