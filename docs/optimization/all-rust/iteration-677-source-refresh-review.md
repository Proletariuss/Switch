# Iteration 677 管理目录权限与并行测试来源审阅

3.48/build348；530条选择性来源，不是产品分母。i676 snapshot `8f9108ee440801735ffa0366eb30f391ec6182a1619ac1d66a242266e7eaf0b2`；i677 snapshot `b9506f9b70b4090e09aceed9974c73430a9d6d86c3e038005f40dc4cc0ec46b3`。

完整source-identity域3925项与保留i676来源 `3218f59ceb392e1a2cddd177cbf25ca3d823b32cc463f23e4dd3cb6ada78e25e` 比较，pre-metadata digest `ead040c24adc2efd6c2799fc72d4aabff8e2d765e09362498a77c619ba195484`，仅审阅两处 managed-control 变化，无路径新增/删除或mode/kind/symlink漂移。

- `rust/crates/proletariusd/src/call_mode_adb/managed_control.rs`：Reviewed newly created STORE path hardening under strict process umask: retain O_NOFOLLOW and inode/owner verification before chmod 0700; existing unsafe directory remains rejected. Focused strict/normal tests only, full cold/App pending.
- `rust/crates/proletariusd/src/call_mode_adb/managed_control/startup.rs`：Reviewed startup fixture parent initialization lock and explicit 0700 searchable permissions, preserving child isolation. New shared-parent regression plus original four tests passed under strict and normal umask; complete cold/App pending.
- `scripts/check-acceptance-ledger.py`：Advance active i677 snapshot and QMI identities, select two reviewed managed-control source pins and eight immutable ancestors. Generic validators remain unchanged; no acceptance promotion.
- `scripts/tests/test_acceptance_ledger.py`：Retain 89 ledger tests; select active i677 and immutable i676 predecessor, with eight-ancestor chain [676,675,672,671,670,669,668,666]. No product or carrier acceptance promotion.

Successor to immutable i676 nine-file 59-anchor finite QMI trace. Full 3925-entry source-domain comparison admits only two reviewed managed-control startup/permission changes outside QMI. Nine raw QMI files and 59 anchors remain byte-identical; no QMI route or owner claim changes. Failed i676 cold capsule remains historical. Strict and normal umask focused PASS proves targeted fixture/control paths only, not a complete cold build or final App. Six software exclusions, productfalse and denominator-unfrozen remain unchanged.

i676同源 cold 的四项启动测试 EACCES 失败且无 App，原始失败证据保留；新严格和正常 umask 定向测试仅证明聚焦路径。元数据 stage 的 ledger89项负测、audit self-test 和契约检查各自证明其边界。

继承677→676→675→672→671→670→669→668→666，26份历史文档保持。新i677 full cold、App、同daemon lifecycle/handoff、B01–B12 closure、offline repeat及最终review/attestation仍待实际同源执行。

P0未闭合，P1–P4验收中，P5/P6开放；0PASS/5PARTIAL/110NOT_REVIEWED，productfalse、分母未冻结。无硬件/网络/DMG操作或功能PASS。
