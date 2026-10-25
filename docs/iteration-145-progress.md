# Iteration 145：模块 Profile 备注 Rust 接管

日期：2026-10-08。目标仍为按全 Rust 后端方案持续推进；本轮只在 `/Users/proletarius/Desktop/Switch` 内更改。

`GET/PUT /api/esim/module-notes` 和模块 ME 电话簿 AT 事务现由 Rust `module_radio` actor 直接处理。GET 选择 ME 存储、读取容量并按最多32个槽位分批查询；PUT 沿用备注更新、删除和选择首个可用槽位的行为。每条 AT action 使用固定命令类型，进入共享 radio actor 的独立 reservation，与 eSIM/Profile 操作互斥，执行前检查通话状态，整体限时55秒。写入结果不确定时不自动重放。请求体继续受1 MiB限制；公开方法表包含 GET、隐式 HEAD 和 PUT。

移除了 Go 公开 handler、`moduleNotesMu` 及 ME 读写事务，并删除 Go→Rust 的模块备注 codec RPC 与对应 UDS namespace。普通 `/api/internal/profile-notes` 手机号关联 RPC仍保留，供读卡器号码历史使用，与 ME 模块备注无关。Rust 对 ICCID 增加安全字符验证，避免不可信值被拼接到 AT 记录。

验证结果：

- `cargo +1.98.0 fmt --all -- --check` 与 Rust workspace 测试通过：852 passed、3 ignored。
- Go `go vet ./...`、`go test ./...` 通过。
- 载波回放2722项、USB状态、radio policy、HTTP/HEAD wire parity、历史PCMA fixture、SDP和AMR payload parity通过。
- Rust迁移契约扫描一致，工具自测42项通过。更新后的路由清单为278条method/path（Rust 202、Go 76，含32条HEAD alias）、127个Swift HTTP调用点、226个审阅范围。
- `--require-rust-only`仍如预期失败：默认Go fallback尚存，141条路由仍有Go所有权或依赖。
- 全门禁首次运行发现冻结 Go HTTP 方法夹具未包含旧模块备注 GET/PUT；补入旧合同样本后重新执行。随后恢复旧 `esim_operation_busy` 错误体的 `recoverable` 字段，新增固定 payload 断言，并重跑最终全门禁，852项 Rust 测试通过。日志位于 `build/optimization/iteration-145/migration-gates-final.log`。

呼入卡片的接听/挂断按钮按整组相对卡片居中；这项 UI 修正已在现有 2.93/build293 Release 中，不计入迁移进度。本轮没有重新构建 App 或生成 DMG，没有访问硬件、号卡、短信或生产用户数据。

本轮保守计入 P2B +0.025 计划人周等值：4.650→4.675/8.0（58.44%）；总进度13.145→13.170/47.5，约27.7%（估算误差±3个百分点）。实机验收、Go-free构建/运行及最终退出仍未完成。
