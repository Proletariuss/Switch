# Iteration 162：短信转发数据源解耦与 IMS OPTIONS 事务校验

日期：2026-10-08。所有改动均在 Switch 项目目录内。本轮没有读取生产短信或密钥，没有连接读卡器/SIM或访问真实代理、ePDG、IMS和运营商网络；没有构建App或DMG。

Rust设备短信转发producer现在直接合并读取Rust持久SMS归档和原生USB短信预览，不再等待Go子进程快照确认，也不依赖Go availability generation。模块与IMS短信必须先写入耐久归档后才能确认接收；未采用的USB短信仍由Rust有界预览提供。转发沿用原有Swift handoff、稳定通知ID、启用/恢复基线、无回填策略、批次上限及队列/ledger原子提交。Rust只取消Go快照作为转发读数据源的依赖；兼容inbox HTTP路由仍保留给短信页面。

全仓检索确认旧的进程内Go快照producer/代次保护器没有生产调用者，仅被测试模拟；现已删除这段失效辅助实现，保留Rust原生producer及公开兼容HTTP入口。`device_sms_forwarding::native_archive_source_runs_without_go_child_snapshot` 和转发启停/基线/准入测试继续验证Rust来源经同一耐久账本工作。

IMS OPTIONS最终响应现在在改变心跳在线状态前由Rust验证SIP帧、Call-ID、CSeq和OPTIONS方法。没有Rust校验通道、关联事务不匹配或方法不匹配时失败关闭。Go仍发送OPTIONS并管理socket、响应waiter、超时/重试、readiness及会话生命周期。

验证通过：

- `bash scripts/test-migration-gates.sh`：Rust workspace和daemon测试通过（daemon 947通过、3忽略），Go全仓`test`与`vet`通过。
- 同一迁移门禁通过2,722项carrier replay、USB/radio/codec/HTTP-HEAD parity。
- `python3 scripts/tests/test_rust_migration_contracts.py`：43项通过；`python3 scripts/check-rust-migration-contracts.py --diff`通过，契约清单291项（Rust224、Go67，含37个HEAD alias）、127个Swift调用点、272个审阅范围。
- `python3 scripts/test-multistandby-integration.py`：18项独立Rust/Go进程集成检查通过。
- Rustfmt、`git diff --check`通过。
- 删除未被生产调用的Go快照producer辅助实现后，重新运行 `cargo +1.98.0 test --manifest-path rust/Cargo.toml -p proletariusd`：947通过、3忽略；Rustfmt和差异空白检查再次通过。全量迁移门禁是在这项无调用者清理前通过的。

Rust-only退出门禁仍因默认Go fallback与147条Go owner/dependency route受阻；未进行实机或真实IMS/provider验收。当前没有生成DMG。

按既定47.5计划人周估算，保守分别计入P2A和P3各`+0.025`：P2A为3.835/5.5（69.73%），P2B为4.925/8.0（61.56%），P3为1.565/8.0（19.56%），P4为0.475/4.5（10.56%），eSIM/LPA为1.125/5.5（20.45%）；总体13.670/47.5，约28.8%（估算误差约±3个百分点）。这不代表VoWiFi/IMS在线生命周期、硬件来源或通知真实投递已完成。
