# Iteration 675 startup-loopback冷门禁修复来源审阅

3.48/build348；530条选择性来源，不是产品分母。i672 snapshot `4d0e2144180823dfe23948f807dba94156162d32d7ddbd77a13e6ad76f2461bc`；i675 snapshot `4a3895694ad264a4ad95f3a91817afcb637a60a91b04538ad796b6347b7d2ffe`。

完整source-identity域3919项与保留i672来源 `2b9e61949ce3b5f999e8c2192aab4dcedf881ad8b3d02148d0593836ecd9eee8` 比较，pre-metadata digest `bc65e98c5f41899fe2a9c3f1dc985618b7b9490bde3e6fceb309c93d0081811c`，仅审阅的startup-loopback测试修复变化，无路径新增/删除或mode/kind/symlink漂移。

- `rust/crates/proletarius-vowifi/src/startup_loopback.rs`：Exact test-only IKE rekey fixture diff reviewed: freeze and verify retransmit ciphertext/SPI and DELETE header/payload; schedule AUTH_LIFETIME for lost-response case after baseline ESP; extend timer windows for 6-second lifetime under parallel load. No production module or QMI anchor changed; this focused repair is not complete cold or carrier acceptance.
- `scripts/check-acceptance-ledger.py`：Advance active i675 snapshot/QMI identities, reviewed startup-loopback raw pin and six-ancestor declarations. Generic semantic validators and canonical set ordering remain unchanged; no acceptance promotion.
- `scripts/tests/test_acceptance_ledger.py`：Retain all 89 existing ledger tests; select active i675, immutable i672 predecessor and ancestry [672,671,670,669,668,666]. Existing no-addition source-drop negative mutation remains meaningful; loops verify all 20 historical document identities.

Successor to immutable i672 nine-file 59-anchor finite QMI trace. Full source-domain comparison admits only a reviewed startup-loopback test fix outside QMI. All nine raw QMI sources and all 59 exact fragment/range pairs remain byte-identical. No QMI route, typed operation, consumer or cleanup behavior change is inferred. Swift default remains AT; QMI consumers and hardware interoperability remain unresolved. Failed i674 cold history is retained, not relabeled PASS. Six software exclusions, productfalse and denominator-unfrozen remain unchanged; no complete cold or functional PASS is inferred.

i674失败cold原始日志与当时全部胶囊保留；286通过/7失败不能改记成功。修复后定向证据由snapshot validation_evidence精确绑定；元数据stage的ledger89项负测、audit self-test和契约检查只证明各自边界。

继承675→672→671→670→669→668→666，20份历史文档保持。新full cold、App、同daemon lifecycle/handoff、B01–B12 closure、offline repeat及最终review/attestation仍待实际同源执行。

P0未闭合，P1–P4验收中，P5/P6开放；0PASS/5PARTIAL/110NOT_REVIEWED，productfalse、分母未冻结。无硬件/网络/DMG操作或功能PASS。
