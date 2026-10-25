# Iteration 300：迁移验证门禁改为遵循 Switch 工作区

修复 Go/Rust parity fixture 的输出边界硬编码到 `Switch_MAC/build`、以及 N待/IMS IPC 集成脚本硬编码旧工作区根目录的问题。Parity fixture 现在根据 `SWITCH_BUILD_DIR` 做路径包含检查；集成脚本从自身源码路径推导 Switch 根目录，并校验构建/运行目录都位于该根目录内。更新 Switch 内 `AGENTS.md` 的工作区约束为 `/Users/proletarius/Desktop/Switch/`。这些改动不改变产品运行逻辑或 Go/Rust owner，不增加迁移进度。

验证通过：完整迁移门禁，Rust workspace 1189 passed / 3 ignored；Go `vet` 和全仓测试通过；Rust/Go 载波回放 2722 例、USB 状态、Radio、HTTP method/HEAD、SDP、AMR、MESSAGE 路由差分均通过。Go 固定 1.26.3 工具链自动下载遇到代理 EOF，因此该轮使用本机 1.26.7（高于 `go.mod` 最低版本 1.26.3）；EVS 静态归档存在于 Switch/build。独立 N待/IMS 跨语言 Unix IPC 集成测试通过33项；合同清单通过，334条契约（Rust273/Go61）、127个 Swift 调用点、326个审阅范围。`cargo fmt --check`、`git diff --check` 通过。未操作 SIM/读卡器、未发短信/拨号、未生成 DMG。Rust-only结构退出审计仍阻塞于7类：Xcode构建Go、App嵌入Go二进制、Rust监督Go、Rust转发到Go UDS、Swift保留Go兼容选择、Go module仍在、256个生产Go源文件仍在。当前退出审计不是通过项，目标未完成。总进度保持 **31.4%**。
