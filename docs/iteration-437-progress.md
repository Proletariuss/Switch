# Iteration 437：同步运营商选择器展示元数据

日期：2026-10-13

## 结果

按上游仓库最新提交 `e5604370f93b863ee212dd3cdc5294979f11bca4` 更新 Switch 的 carrier registry。数据总量仍为 **683 profiles / 853 selectors**；变化限于 10 个 selector 文件的 `carrier_name` 展示字段，匹配条件、IKE/ePDG/IMS/ESP 参数均未变化。完整 1,536 个 profile/selector 文件 SHA-256 锁校验通过。

Rust registry parser 与运行时 carrier profile保留被选中 selector 的展示名；Go兼容 resolver使用相同语义，未将该字段用作 SIM 匹配依据。Rust 与 Go 各有回归测试确认标签从选择器到运行时 profile 的传递。旧 d46fc4e manifest 保留用于可重现对比，新锁见 [e560437 manifest](optimization/all-rust/carrier-reference-locks/ios-carrier-profiles-e5604370f93b863ee212dd3cdc5294979f11bca4-manifest.json)。来源说明见 [carrier data SOURCES](../backend/internal/vowifi/carrier/data/SOURCES.md)。

## Clarivate_IOS 交叉核对

只读核对 `/Users/proletarius/Desktop/Clarivate_IOS`：其 683 个 profiles 与 Switch 当前 profiles 完全一致；selectors 仅上述 10 个文件不同，Clarivate 本地仍为 d46fc4e。该项目最近的 iOS Reader/B0/B1 与稳定性变更属于 Flutter/iOS 读卡器和生命周期路径，不包含新的 carrier IMS 配置；不移植这些平台代码。Clarivate 的 `3_hk.json` 属于 app-owned override，结构及哈希与 Switch 不同，未覆盖 Switch 文件。

## 验证

- `go test ./internal/vowifi/carrier -count=1`：通过。
- `cargo +1.98.0 test --manifest-path rust/Cargo.toml -p proletarius-vowifi`：182 tests通过（crate 180、integration 2），无失败。
- carrier source lock：1536/1536 文件 SHA-256通过。Clarivate 对比：profile差异0，selector差异10。
- `scripts/test-migration-gates.sh`：通过；[完整日志](../../build/reports/iteration437-migration-gates.log)，SHA-256 `745eafe91e7658dcedd8d625cdcce03d0ec5e58afabb69285963aa97cd0407b6`。
- API契约 `--diff`：通过，354 method/path（Rust293/Go61）、127 Swift调用点、378 review scopes。
- 无硬件、SIM、运营商网络测试；不据离线配置测试宣称VoWiFi实网成功。

本轮没有降低 Go runtime/build 依赖类别，也没有改变全Rust迁移里程碑，因此整体进度维持 **37.99%（18.045/47.5）**。当前 DMG 3.14/build314 冻结于 Iteration435，不包含此次源码与数据更新；本轮未生成 DMG。
