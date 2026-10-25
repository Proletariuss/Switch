# Iteration 146：eSIM 电话簿探测接管与呼入按钮居中编译修正

日期：2026-10-08。所有修改与验证材料均在 `/Users/proletarius/Desktop/Switch/` 内；全 Rust 后端仍是持续目标，本轮未生成 DMG。

`POST /api/esim/phonebook/probe` 现在由 Rust `module_radio` actor 处理。删除 Go handler、旧 AT 探测 helper 和对应 Go 测试；Rust 使用受限的 typed command，在共享 radio actor reservation 下执行固定 `AT+CPBS=?`、选择 `ME`、查询 `AT+CPBS?`、`AT+CPBR=?` 与 `AT+CPBW=?`。接口保留既有 JSON 字段、响应内容、原命令顺序及部分失败继续规则；ME 不支持时停止，物理结果不确定时禁止继续发 AT 命令。与 eSIM/Profile 操作互斥，忙碌时保持既有 `esim_operation_busy` 与 `recoverable:true` 响应。无模块时仍返回旧响应结构。测试没有访问读卡器或 SIM。

呼入接听/挂断按钮容器按 GeometryReader 提供的卡片可用宽度定位到中心。为让 SwiftUI 在闭包离开当前栈后仍安全构建，闭包参数标记为 escaping；正式 Release 构建脚本已更新 `dist/Switch.app` 并通过 `codesign --verify --deep --strict`。未制作 DMG。

验证：Rust radio 定向组 **39通过、1个既有外部 fixture 测试忽略**；Rust workspace **855通过、3忽略**；Go 全仓 `go test ./...` 和 `go vet ./...` 通过。载波、USB 状态、radio policy、冻结 Go HTTP 方法/HEAD、PCMA 历史 fixture、SDP 与 AMR parity 门禁通过。迁移契约工具42项自测、普通扫描与 `--diff` 通过。当前278项 method/path（Rust203、Go75，含32个HEAD alias）、127个Swift调用点、226个review scopes。Rust-only gate仍因默认Go fallback及140条Go owner/dependency路由失败。

完整门禁日志：`build/optimization/iteration-146/migration-gates.log`；Release 构建日志：`build/optimization/iteration-146/release-build.log`。`dist/Switch.app` 内 `switchd-rs` SHA-256：`cfc1c938507546b671591d2e7c1724dc8253c49ccc268603e48b757f3dda5321`。本轮未访问真实设备、SIM、通话、短信或第三方网络；9eSIM `6982`仍排除。

本轮保守计入 P2B **+0.025计划人周等值**：4.675→**4.700/8.0（58.75%）**；总体13.170→**13.195/47.5≈27.8%**（估算误差约±3个百分点）。呼入按钮 UI 不计迁移分。读卡器/实体卡验收、Go-free构建与运行、Go最终退出仍未完成。
