# 当前可验证基线：软件证据继任记录（2026-10-23）

本记录是 [`current-baseline-scope-decision.json`](current-baseline-scope-decision.json) 的证据层继任，机器可核对的映射在 [`current-baseline-scope-successor-2026-10-23.json`](current-baseline-scope-successor-2026-10-23.json)。前件保持 2026-10-22 采纳时的原始状态；其中 i663 / 3.46 的已验证切面与当时“3.48 尚未构建”的叙述是历史事实。当前记录绑定新的 i677 / 3.48 切面，不回填或重写前件。

| 正式历史叶 | 当前证据轨道 | 新核实的软件事实 | 正式台账状态 |
| --- | --- | --- | --- |
| `P0-1.working-baseline` | `P0-1.current-source-baseline` | i678 在隔离位置从 i677 冷构建审计的完整 manifest 恢复 3,928 个当前源码文件，逐字节、mode 和完整 source identity 核对；恢复副本已删除 | `NOT_REVIEWED`，未授予功能 PASS |
| `P0-1.baseline-280` | `P0-1.current-build-baseline` | i677 同源 3.48/build 348 冷构建、App 与无设备软件验收完成；发布清单绑定现存 DMG SHA-256 | `NOT_REVIEWED`，未授予功能 PASS |

两条新 ID 是**证据轨道**，尚未写入正式 115 叶台账。此继任不把 2.80 源码、实际切换前工作区或旧版升级兼容性视为已恢复；当前构建也不替代这两条历史叶原有的必需条件。若未来审定正式替换、增删或拆分叶子，需另行发布带旧新映射、适用场景与分子/分母影响的追加版本；本记录没有做该项裁决。

所绑定的**已验证构建切面**源码身份为 `b50cd9f5641e1d1d9006b64dc3f9f107272b1ae750bab89a20c511f017073c33`。冷审计完整 manifest 有 3,928 项，规范化数组 SHA-256 为 `c01e243f8185f2909b98dde825cb183173c64f1d0b55866d4bcb70dd43d60f0e`；i678 回执记录 206,220,050 字节的恢复及原始字节比较。3.48/build 348 的无设备软件验收报告为 `build/reports/iteration-677-no-device-software-acceptance.json`，DMG SHA-256 为 `821843a9c793c930078c7a624d1577b87bacd81666d744d122f186dab969d750`。新增本继任文件后，工作树身份会变化；这些散列只证明 i677 冻结切面，不宣称随后工作树与该构建字节同源。新校验器逐项复核证据，继任文件本身不会改变既有证据。

验收边界仍是软件。DJI/读卡器实机、卡与运营商互通、QMI/MBIM 实链路、短信/通话、物理音频、外部投递、GUI 运行态和旧版回滚尚无本轮产品验收结论。P0 未闭合，P1–P4 验收中，P5/P6 开放；历史 P0 与当前轨道均为 0 个功能 PASS，整体产品未完成，正式分母未冻结，因此没有整体完成百分比。

在 `src` 目录运行 `python3 scripts/check-current-baseline-successor.py`，可只读复核前件、115 叶台账、i677 审计与软件报告、i678 恢复回执，以及现存发布清单和 DMG 的绑定。该检查不构建应用、不挂载 DMG，也不触及设备、卡、网络或用户数据。
