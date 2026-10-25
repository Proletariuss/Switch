# Iteration 140：USBNet 模式接口迁入 Rust

日期：2026-10-08  
工作区：`/Users/proletarius/Desktop/Switch/src`

## 改动

`POST /api/network/usbnet` 现在由 Rust `module_radio` 直接处理，Go mux 注册及 `setUSBNetMode` handler 已移除。Rust 严格限制模式为 0 或 1，服务端固定生成 `AT+QCFG="usbnet",0/1`，通过共享射频 reservation 与串行物理队列执行，命令超时为 8 秒；成功或底层失败后都会尝试释放本次 reservation。保留旧成功 JSON 字段、400 参数错误和 502 底层错误形状，并将请求体限制为旧 `decodeJSON` 的 1 MiB。没有允许客户端提交任意 AT 命令。

新增离线夹具测试覆盖缺省/null 模式、未知字段与错误类型、精确命令、成功响应和传输失败后的 lease 释放。迁移检查还发现旧 Go ServeMux 方法样本缺少多组已迁移接口；现按历史公开路由补齐冻结样本，并恢复 `call-mode/backups/delete` 与 `export` 的旧 405/Allow 策略。Go 生成的独立 ServeMux 与 Rust 对照 **350 个 HTTP/HEAD wire case**，另有12个原样回落 case。

## 验证

- `scripts/test-migration-gates.sh`：Rust workspace 通过；`proletariusd` **830通过、0失败、3忽略**；Go `go vet ./...` 与全仓测试通过；载波配置回放2,722项、USB状态/射频投影/HTTP方法对照及 PCMA/SDP/AMR codec 对照通过。
- Go `cmd/proletarius-macos` race 测试与 `go vet`：通过。
- 迁移契约工具 **42项通过**；census 一致：276条 method/path（Rust197、Go79，含30个 HEAD alias）、127个 Swift HTTP 调用点、224个审阅范围。
- Rustfmt、`git diff --check`：通过。
- Rust-only 路由退出门禁仍失败：默认 Go fallback 存在，另有148条路由仍含 Go owner/dependency。
- Release `Switch.app` **2.92/build292 arm64** 构建成功，deep strict 签名校验通过；体积约83 MB。内嵌 `switchd-rs` SHA-256：`db834cff70ad84ff22f2b344ebec887bb1983eabf284c8b9986d5f9c930951c5`。

本轮只使用模拟 transport 与离线 parity fixtures；没有向读卡器发送 QCFG 命令、重启模块、访问实体 SIM、发送短信或发起呼叫。没有生成 DMG。号码问题遵照用户指示不再排查。

## 进度

保守计入 P2B **+0.025计划人周**：4.575 → **4.600/8.0（57.50%）**。总体由13.045增加至**13.070/47.5≈27.5%**（估算误差约±3个百分点）。Rust-only构建/运行、其余Go路由、设备级和长期稳定性验收仍未完成。
