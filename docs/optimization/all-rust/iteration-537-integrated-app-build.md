# Iteration 537：当前 SwiftUI 与 Rust 后端的一体化 macOS 构建

日期：2026-10-18。

从当前 `project.yml` 重新生成 Xcode 项目，在 Switch_MAC 的 `build/` 下执行 macOS arm64 Release 构建。Xcode 最终返回 `BUILD SUCCEEDED`；构建脚本同步编译当前 Rust `proletariusd` 并嵌入 `Switch.app`，未创建 DMG、未安装或启动 SwiftUI 应用。

对实际构建产物核对：

- SwiftUI 主程序、`switchd-rs` 和随包 libusb 均为 arm64；`Contents/Resources/backend/` 仅有 `switchd-rs` 与 `libusb-1.0.0.dylib`。
- 整个应用包通过 `codesign --verify --deep --strict`；Rust 后端对 libusb 的引用是 `@loader_path/libusb-1.0.0.dylib`。三个 Mach-O 均无已知 Go 构建标记，包内没有 `.go` 文件。
- 从该应用包**内**的 `switchd-rs` 在隔离数据目录以不存在的串口路径启动，`/api/v2/runtime` 返回 `backend=rust`、`hardware_owner=rust`、`default_runtime=rust-native`、`go_backend_connected=false`、`go_backend_phase=retired`；健康接口为 200。SIGTERM 后进程零退出、所有测试 socket 消失，日志有原生退出完成回执。
- 本轮再次通过 Go 后端退出结构门禁和 `git diff --check`。

脱敏证据位于 `build/reports/iteration-537-app-build.log`、`iteration-537-bundle-audit.json`、`iteration-537-bundled-daemon-smoke.json`。产物为 `build/iteration-537-derived/Build/Products/Release/Switch.app`，版本仍为 3.43（构建 343）；该目录是验证产物，不是新发行版。

这是当前机器的增量工具链构建与无设备进程验证，**不证明**干净机器重建、已安装旧版数据升级/回退、SwiftUI 实际启动、硬件/运营商业务、SIM World IMS/SMS/通话或长期运行。隔离运行时 `cutover_ready=false` 是预期的无设备结果。旧版已安装 daemon 未被替换或停止。后续仍需完成计划中的产品与设备验收；本轮未生成 DMG。
