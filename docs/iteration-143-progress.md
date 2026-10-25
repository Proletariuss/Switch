# Iteration 143：模块 Profile 备注编解码迁入 Rust

日期：2026-10-08。全 Rust 后端目标持续实施；源码与验证材料位于 `/Users/proletarius/Desktop/Switch/`。

## 实施

模块 Profile 备注格式的纯计算部分已从 Go 移入 Rust 私有 UDS codec。Rust 解析 CPBR 条目、按 ICCID 关联备注、保留重复条目处理规则，并提供 URL-safe unpadded Base64 编解码；同时保留输入修剪、Unicode 字符长度、编码后 255 字节限制，以及 Go JSON 对无效 UTF-8 的替代行为。私有 `/api/internal/module-profile-notes/decode` 与 `/encode` 仅接受经过认证的精确 POST action。

公开备注 GET/PUT、Profile/读卡器选择、读卡器 AT 命令和硬件事务继续由 Go 负责。本轮没有迁移公开路由，也没有增加设备 I/O。

另修正通话页呼入时的接听/挂断按钮偏左问题：动作行先占满卡片宽度，再将固定尺寸按钮组叠加在整行中心。原按钮尺寸、间距、样式和动作保持不变；此 UI 工作不计入 Rust 迁移进度。

## 验证

- Rust module-profile-notes codec 专项：6项通过；Go adapter 专项：4项通过。
- `scripts/test-migration-gates.sh`：通过；Rust workspace **843通过、0失败、3忽略**，Go 全仓测试、race、`go vet`、HTTP/HEAD 与协议兼容检查通过。
- 迁移合同工具：42项自测通过；`check-rust-migration-contracts.py --diff` 一致。清单为279条 method/path（Rust201、Go78，含31条HEAD alias）、127个Swift调用点、226个审阅范围。
- Rust-only 路由退出门禁仍失败：默认 Go fallback 存在，143条契约仍有 Go 所有权/依赖。该失败保留为真实未完成项，没有将清单一致误报成全 Rust。
- 通话页修改通过 macOS Debug Xcode build。
- Release `Switch.app` **2.93/build293 arm64** 构建和 `codesign --verify --deep --strict` 验证通过。内嵌 `switchd-rs` SHA-256：`8813532b694a3e4d46f559784566dfcecd4a6a5eac0c7e83f96a724f04b30841`。
- APIClient超时及短信转发source-guard审阅指纹经复核更新；契约差异检查随后通过。
- 没有连接或操作模块、读卡器、SIM；未呼叫、未发短信、未写卡，也没有生成DMG。

验证记录：`../build/optimization/iteration-143/` 下的 `migration-gates.log`、`release-build.log`、`contract-tests.log`、`contract-diff.log`、`contract-inventory-refresh.log`、`rust-only-gate.log` 和 `diff-check.log`。

## 进度

此项只迁移 Profile 备注 codec 计算，不代表 eSIM 下载或公开硬件路径完成。按原计划保守计入 P2B **+0.025计划人周等值**，至 **4.650/8.0（58.13%）**；总量 **13.145/47.5≈27.7%（估算误差约±3个百分点）**。P2A为 **3.760/5.5（68.36%）**。Go-free构建、Go运行时退出、设备与长稳验收仍未完成。
