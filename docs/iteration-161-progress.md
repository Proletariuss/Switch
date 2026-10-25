# Iteration 161：VoWiFi 代理诊断入口迁入 Rust

日期：2026-10-08。仅在 `/Users/proletarius/Desktop/Switch` 工作；没有读取生产短信或密钥，没有连接读卡器/SIM，也没有访问真实代理、ePDG、IMS或运营商网络。本轮未构建App或DMG。

`POST /api/reader/vowifi/proxy-check` 现在由 Rust daemon 处理。它复用 Rust 现有 SOCKS5 本地握手探测和 SWU 传输选择逻辑，保留原有 JSON 字段与空代理错误响应。出口IP归属查询仍是可选项，有4秒超时和64KiB响应上限；该可选查询失败不影响本地代理检查。接口明确不把握手或传输配置结果描述为 ePDG UDP 往返、IKE/IPsec 隧道或 IMS 就绪。

Go公开路由注册、handler及重复的 SOCKS5/HTTP探测代码已删除；Rust注册公开路由并声明 owner。没有增加 method/path 契约。集成检查验证 Go 旧响应语义仍兼容。

验证通过：

- `cargo fmt --manifest-path rust/Cargo.toml --all`
- 定向 Rust 代理检查测试：6项通过；使用本地 fake SOCKS5 服务，没有真实互联网请求。
- Go公开HTTP方法/路由兼容测试通过。
- `python3 scripts/tests/test_rust_migration_contracts.py`：43项通过。
- `python3 scripts/check-rust-migration-contracts.py --diff`：通过；当前清单291项（Rust224、Go67，含37个HEAD alias）、127个Swift HTTP调用点、272个审阅范围。
- `bash scripts/test-migration-gates.sh`：通过；Rust daemon 946项通过、3项忽略，Go全仓测试与`go vet`、2,722项carrier replay及USB/radio/codec/HTTP-HEAD parity通过。
- `python3 scripts/test-multistandby-integration.py`：18项真实Rust/Go独立进程集成检查通过。
- `git diff --check`：通过。

Rust-only退出门禁仍因默认Go fallback与148条Go owner/dependency route未通过。代理诊断入口迁移不代表其下游网络或VoWiFi会话已经迁入Rust，也不构成实机或运营商验收。

保守计入P2A **+0.025计划人周等值**，由3.785/5.5升至**3.810/5.5（69.27%）**；总体由13.595升至**13.620/47.5≈28.7%**（估算误差约±3个百分点）。P2B为4.925/8.0（61.56%），P3维持1.540/8.0（19.25%），P4为0.475/4.5（10.56%），eSIM/LPA为1.125/5.5（20.45%）。


首轮完整测试曾出现一次 IMEI 查询测试的临时锁文件断言失败；该测试独立复跑通过。为消除时间戳 ID 在并发调用时可能相撞的风险，`new_instance_id`现加入进程内原子序号，并增加并发唯一性测试。最终 Rust daemon 全套测试为947通过、3忽略，完整迁移门禁通过；这项测试隔离加固不另计迁移进度。
