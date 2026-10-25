# Iteration 672 capsule App inventory来源审阅

3.48/build348；530条选择性来源，不是产品分母。i671 snapshot `a83de8149abbc31d16d321d6eb8772b4d4aca4379465991e41c6631adf880eb1`；i672 snapshot `4d0e2144180823dfe23948f807dba94156162d32d7ddbd77a13e6ad76f2461bc`。

完整source-identity域3916项与保留i671来源 `8665fe0297c7aef4eea4ba28929071709360c7963d28f028227ac124e6ac7277` 比较，pre-metadata digest `a9722cadaefb2d30d93096f113162739e43e73810a1afa0ac20f0494e0e58b3b`，仅audit-clean producer/self-test变化，无路径新增/删除或mode/kind/symlink漂移。

- `scripts/audit-clean-build.py`：Compared entire file against immutable i671 source: app_bundle_inventory gains an explicit optional trusted inventory root, resolves the App beneath that root and fails closed on outside-root/symlink escape. verify(capsule) passes capsule so artifact App inventory path is dist/Switch.app; default callers retain the prior workspace-relative path. Existing consumer strict path requirement, regular-file/symlink/byte/mode/directory/tree checks remain unchanged. Embedded self-test covers default compatibility, capsule-relative output and direct/symlink escapes. Producer self-test plus 33 focused Python tests passed on exact pre-metadata source; no new complete cold, device or product acceptance is inferred. Earlier hardlinked i671 stage drift was repaired with recorded before/after evidence and both successful stages fully byte-isolated while preserving their original source digests.
- `scripts/check-acceptance-ledger.py`：Advance only active i672 snapshot/QMI identities, exact audit-clean producer raw pin and five-ancestor declarations. Canonically ordered set declarations remove hash-dependent serialization. Generic semantic validators remain unchanged; no acceptance promotion.
- `scripts/tests/test_acceptance_ledger.py`：Retain all 89 existing ledger tests; select active i672, immediate i671 and ancestry [671,670,669,668,666]. Existing source-drop negative test now selects a declared changed file when there are no newly added pins, keeping its mutation meaningful for no-addition successors. Existing loops verify all 17 historical document identities without per-iteration test families.

Successor to immutable i671 nine-file 59-anchor finite QMI trace. Full source-domain comparison admits only the cold artifact-audit inventory producer/self-test change outside QMI. All nine raw QMI sources and all 59 exact fragment/range pairs remain byte-identical. No route, typed operation, consumer or cleanup behavior changes. Swift default remains AT; QMI consumers and hardware interoperability remain unresolved. Failed i670 cold history is retained. Software exclusions, productfalse and denominator-unfrozen remain unchanged; no full cold or functional PASS is inferred.

仅producer显式使用可信capsule根；默认调用保持原工作区相对路径。consumer严格`dist/Switch.app`校验、bundle inventory模式/字节/目录hash和symlink拒绝保持，未把artifact审计或接受方判定弱化。执行证据由snapshot validation_evidence精确绑定；元数据stage的ledger89项负测、audit self-test和契约检查只证明各自边界。

继承672→671→670→669→668→666，17份历史文档保持。i670完整cold仍失败；i671独立恢复/发布receipt只证明当时pre-metadata synthetic套件，未当作本轮full cold。新full168/336、App、同daemon lifecycle/handoff、B01–B12 closure、offline repeat及最终review/attestation仍待实际同源执行。

P0未闭合，P1–P4验收中，P5/P6开放；0PASS/5PARTIAL/110NOT_REVIEWED，productfalse、分母未冻结。无硬件/网络/DMG操作或功能PASS。
