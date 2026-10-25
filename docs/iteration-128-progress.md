# Iteration 128 — 模块语音运行时部署文件校验与读取迁入 Rust

日期：2026-10-08。

通话模式部署原先由 Go 从 `voice-runtime/<version>` 目录读取驱动文件并交给 USB ADB。现在新增仅限内部凭证的 Rust `GET /api/internal/call-mode/runtime/deployment-files` 接口。Rust 将路径固定在自身数据目录，拒绝符号链接目录和文件，以 no-follow 文件描述符打开普通文件，校验预期长度和 SHA-256，再在 2 MiB 响应上限内返回三个部署文件的字节。Go 验证响应中的版本、顺序、名称、大小、哈希和字节内容后才执行现有 ADB 推送。

迁移边界：Go 仍负责下载和通话模式状态机，保留运行时清单副本；Go 仍拥有 ADB USB 接口、root shell、内核/音频路由自检和部署生命周期。本轮只迁移本地部署文件的可信读取，不宣称通话模式工作流或 ADB 已迁完。Rust/Go 两侧仍各自列出三个文件的预期元数据，后续应收敛为一个共享清单。

离线验证：Rust daemon **770通过、0失败、3忽略**，包含有效文件、篡改哈希、文件/目录符号链接拒绝；Go `CallModeRuntimeClient` race 用例通过，覆盖内部 token、Rust 拒绝和 Rust 不可用时失败关闭；`go vet ./cmd/proletarius-macos` 通过。迁移合同工具 `--diff` 通过，清单 **265条**（Rust **185** / Go **80**，含30条 HEAD 别名）、**127个** Swift 调用点、**210个**人工审阅范围；脚本测试 **49项**通过。Rustfmt、`git diff --check` 通过；`scripts/build.sh` 构建 `dist/Switch.app` 2.86/build 286 arm64，deep strict 签名验证通过。

未操作读卡器或实体卡、未运行真实 ADB 部署、未生成 DMG。P2B 计入 **+0.025计划人周**，由 **3.975**升至 **4.000/8.0（50.00%）**；总体为 **12.470/47.5≈26.3%**，估算误差约±3个百分点。Go 运行时下载、ADB/helper、通话模式重启恢复、其他硬件/IMS业务、Go-free构建和实机验收仍未完成。
