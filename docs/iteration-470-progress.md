# Iteration 470：通话音频路由与 USB 语音切换执行迁入 Rust

日期：2026-10-13。将公开的通话音频启动/停止控制从 Go 移交给 Rust，并复用 Rust call-mode ADB/USB 语音切换 owner。此项缩小 Go 公开业务面，但仍保留一个显式 Go readiness 事实适配器，不代表通话栈或全部硬件流程已 Rust-only。

## 变更与职责边界

- Rust gateway 直接处理 `POST /api/call/audio/start` 与 `POST /api/call/audio/stop`，负责方法/请求边界、producer 身份校验、启动响应与停止响应；Go 不再注册这两个公开路由。
- Rust 在启动前通过认证的 `POST /api/internal/call-mode/audio/readiness` 读取现存 call-mode readiness/runtime-version 事实。该兼容适配器严格校验内部令牌与空 DTO；通话模式状态事实本身仍由 Go 提供。
- 音频路由的 ADB 工作由 Rust shared voice executor 执行，串行化 ADB、持有 USB VoiceTransitionLease，并让阻塞 worker 的执行与资源释放跨越 HTTP 取消边界。停止和启动共用同一 owner；状态查询不会误获取切换 lease。
- Go 仍持有模块/IMS readiness 来源、部分 call-mode 编排、SIP 通话事务及音频设备交互；没有宣称 VoWiFi 呼叫端到端迁移完成。

## 验证

- 新增 Rust route 和 shared executor 测试、Go readiness adapter 认证/DTO/未就绪测试、公开 Go mux parity 更新。
- 全量 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,476 passed / 0 failed / 3 ignored**；Go 全仓 `go vet ./...` 与 `go test ./...`；Swift BLE 回归、2,726 项 carrier replay、USB/radio/HTTP method+HEAD/IMS/SDP/AMR/MESSAGE/USSD/dial-target/rejection parity 均通过。
- 定向 VoWiFi carrier Go 与 Rust 回归通过；Clarivate_IOS 今天的未提交更新已只读检查。其 carrier registry 为旧版 683/853，未发现新的运营商模板可移植；Switch 的 685/855 快照与新增 Lebara/Dito 回归保持有效。
- API census：**355** method/path contracts（Rust **295** / Go **60**，含 51 HEAD aliases）、127 Swift HTTP call sites、382 reviewed scopes。合同 `--diff`、`cargo fmt --check` 与 `git diff --check` 通过。
- 全量门禁日志 SHA-256：`84817b14880215dbd4956a3c1e3e7fd904eafafa77bf5ba533c10074aea54eb7`；见[门禁日志](../build/reports/iteration-470-migration-gates.log)。结构退出审计仍有 **6 类**阻塞、**266 个**生产 Go 文件，Rust-only route gate 仍受 Go 依赖阻塞。
- 全部是离线验证；没有调用读卡器、SIM、短信、电话或运营商网络。记录本轮迁移门禁时没有构建 DMG；用户随后单独要求生成，已从 Iteration470 源码重新构建并交付 [Switch-3.15-macOS27-iteration470-rebuild-20261013.dmg](../dist/Switch-3.15-macOS27-iteration470-rebuild-20261013.dmg)，版本 3.15/build315、arm64，签名与镜像校验通过，SHA-256 `e7fc480aee1492ff8fd22ffa1ddef9fc61c195c4cbe60b16dbe27c0ef92cb29a`。该 DMG 不含后续 Iteration471 源码变更。

## 进度

本轮按 P2B 增加 **+0.025 计划人周等值**：公开音频控制入口及其 ADB/USB transition 执行 owner 已迁至 Rust，仍保留 Go readiness 依赖。累计估值 **18.070/47.5 = 38.04%**（误差约 ±3 个百分点）。六类 Go 退出阻塞、IMS/SIP/媒体生命周期、Go-free 构建和实机/运营商验收均未完成；项目目标仍在持续推进。
