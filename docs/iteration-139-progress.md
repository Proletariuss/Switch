# Iteration 139：公开模块重启入口迁入 Rust

日期：2026-10-08  
工作区：`/Users/proletarius/Desktop/Switch/src`

## 改动

`POST /api/network/reboot-module` 现在由 Rust `module_radio` 直接处理。Rust 按单调 `Purpose::Reboot` 序号分配操作身份，在共享射频 owner 中取得临时 reservation，再调用既有、受飞行模式/蜂窝状态策略保护的 `reboot` action；成功及失败路径都会尝试释放本次 reservation。Go mux 中的该路由和公开 handler 已删除；CallMode 经内部 Rust radio adapter 发起的重启路径保留。

成功响应仍为 HTTP 202，包含 `accepted: true` 和 `response`。手动飞行模式开启时保留旧版 HTTP 409、`module_flight_mode_enabled` 错误码、中文提示及 `recoverable: true`。其他 Rust radio 拒绝继续提供同一 coded-error 字段布局与安全恢复标志。接口只接受 POST，其他方法保持 405/Allow 约束。

## 验证

- `cargo +1.98.0 test --workspace --offline --locked`：**1,308通过、0失败、14忽略**；其中 daemon 为826通过、3忽略。
- 模块重启专项：3项通过，覆盖成功执行/释放 reservation、飞行模式门禁和旧错误 JSON。
- Go `cmd/proletarius-macos` race 测试与 `go vet`：通过。
- Rust 迁移契约自测：42项通过；清单一致性通过，共276条接口（Rust196、Go80，含30个HEAD alias）、127个 Swift HTTP 调用点、224个审阅范围。
- `cargo +1.98.0 fmt --all -- --check`、`git diff --check`：通过。
- `--require-rust-only` 仍按预期失败：默认 Go fallback 存在，149条路由仍有 Go owner/dependency。

## 构建和边界

Release `Switch.app` **2.91/build291 arm64** 构建成功，`codesign --verify --deep --strict` 通过。应用路径：`/Users/proletarius/Desktop/Switch/dist/Switch.app`；体积83 MB。内嵌 Rust daemon SHA-256：`aa466ba2caf676752868093c4a5ba60e6dc59a09d860759a3672da46b3092f15`。通话页之前修正的呼入接听/挂断按钮居中布局随此版本保留。

本轮没有实际重启模块、访问读卡器或实体 SIM、发送短信/发起呼叫，也没有生成 DMG。Rust-only构建、其余 Go 路由、设备级与长期稳定性验收仍未完成。9eSIM `6982` 专项继续排除；号码未响铃问题遵从用户最新指示，不再排查。

## 进度

保守计入P2B **+0.025计划人周**：4.550 → **4.575/8.0（57.19%）**。总体由13.020增加到**13.045/47.5≈27.5%**（估算误差约±3个百分点）。呼入按钮等纯 UI 修复不计入Rust迁移比例。
