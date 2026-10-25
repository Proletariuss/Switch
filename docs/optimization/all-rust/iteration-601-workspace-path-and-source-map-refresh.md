# Iteration 601：工作区路径与来源映射刷新

2026-10-20。根据用户明确指示，当前唯一编辑、构建、测试和交付目录为 `/Users/StatTrak/Desktop/Switch_MAC/`，其 `src/`、`build/`、`dist/` 分别存放源码、生成数据和发行物。早期 `/Users/proletarius/Desktop/Switch/` 不再是写入目标，历史迭代记录保留原样。Codex 的目标记录工具只允许读取和更新状态，不提供修改 objective 文本的接口；因此本文件、改造方案首页和 `AGENTS.md` 作为当前项目内的权威路径记录。

本轮复核短信转发启动准入变更：配置模式未确认时返回明确错误；只在明确确认关闭后返回 disabled；retiring/failed 状态继续失败关闭。Rust 设备短信生产者在读取 inbox、建立通知基线、提交消息或推进 durable cursor/handoff 前检查该准入。HTTP 路由与 DTO 未变化。当前源码原始 SHA-256 与 Iteration 598 结构化复核报告一致；生产审阅指纹更新为 `91c569b047b6e62251842ebfbea1a7053e292f8a117eba37b59b93c3979ab547`。

路由清单只发生了源码插入导致的两处行号漂移：`CallAudioStore.swift` 的 `/api/call/audio/stop` 调用从 389 行移到 392 行；`sms_forwarding.rs` 的 device-snapshot 路由源码从 325 行移到 332 行。API/Swift 调用/后台能力数量及路由契约没有变化。旧 Iteration 600 映射原样归档于 [entry-acceptance-map-iteration-600.json](entry-acceptance-map-iteration-600.json)，其 SHA-256 为 `eed6cfcecb4cf200596c5a583017e6170c602390e289e0708c7511870311064e`；当前映射仅刷新两处 Swift/API 行号绑定，未新增需求、实例或功能验收。

## 本轮验证

- `check-rust-migration-contracts.py --require-rust-only`：通过；294 method/path contracts，Rust owner 294，Go owner 0，129 Swift HTTP call sites，470 reviewed scopes。它只证明静态路由清单一致，不证明完整运行时 Go-free。
- `check-acceptance-ledger.py`：metadata consistent；294 API、131 Swift 调用、56 runtime rows、115既有需求叶子、21场景类别。分母仍未冻结，`functional_acceptance_awarded=false`。
- `test_acceptance_ledger.py`：47项通过。
- Rust定向准入测试：启动状态分类1项、未确认配置游标/基线保护3项通过。
- `check-rust-backend-exit.py --require-go-free`：通过；此结构检查不代替应用、迁移恢复或设备验收。
- daemon全量 `cargo test --offline -p proletariusd --bin switchd-rs` 使用正确的 `SWITCH_DATA_DIR` 隔离环境单次运行通过：3036通过、0失败、8忽略，耗时1250.68秒。完整日志为 [`iteration-601-daemon-full-correct-env.log`](../../../../build/reports/iteration-601-daemon-full-correct-env.log)，SHA-256 `176aceb7ff79a39a9ffa61bcd3e1ce8215cbb380c25fba7f128f61084a42e1f9`。

## 当前边界

P0仍未闭合；P1–P4设备与业务验收进行中；P5语音验收仍需真实通话/编码证据；P6最终构建、升级、恢复与长稳验收未完成。接口归属覆盖为294/294（100%），不等于项目完成。验收分母未冻结，不报告总体百分比。入站短信按用户此前指示暂缓；本轮未发送短信或拨号，也未操作读卡器/SIM，未生成新DMG。完整 daemon 软件回归已单次全绿；退出测试后，使用 `scripts/clean-generated.rb` 精确清理 `build/rust-target`，回收分配空间7,184,171,008字节。后续音频清理缺口与验证记录见[Iteration 602](iteration-602-call-audio-stale-retirement.md)。
