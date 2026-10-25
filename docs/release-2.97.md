# Switch 2.97 / 构建 297

2026-10-10，按用户“输出 DMG 文件后再继续推进”的要求发行。

## 发布范围

本次冻结至已通过门禁的 Iteration 322，保留 2.96 的 9eSIM 蓝牙密码握手与 B0 BF71 下载认证，并包含后续 BLE 卡片传输汇入 Rust、业务 capability 与物理占用原子交接、蓝牙控制取消后继续持有实际 I/O 许可等改动。

正在接入的 Iteration 323 读卡器选择/generation 状态迁移未放入本包。主开发源码中的改动保留，发布源码使用独立快照 `/Users/proletarius/Desktop/Switch/build/releases/2.97/src`；对 323 修改进行了逐文件精确还原并重新执行全部门禁，不是直接使用旧二进制。

B1 默认密码派生和自定义密码优先级、认证 ACK 与 eUICC 管理模式门槛保留。B0 hook 在真实 Rust LPA 下载路径中执行，位于获取新 challenge 之后、标准 AuthenticateServer 之前，并要求 BF71 返回 9000。B0 仅针对实现中列出的 EID 范围。以上为代码、合成协议和发行验证，不代表本次已完成 B0/B1 真卡认证、下载或所有 6982 问题的修复。

## 验证与产物

- 完整迁移门禁通过：Rust workspace 1,765 passed / 6 ignored，daemon 1,236 passed / 3 ignored；Go vet、全包测试及现有协议/HTTP/媒体协商差分通过。
- Swift BLE 生产事件状态机 34 组、手动选择 13 项通过；不访问真实蓝牙设备或用户钥匙串。
- 接口契约清单通过：335 method/path，Rust 275 / Go 60，127 个 Swift 调用点，334 个审阅范围。
- Release 构建成功。只读挂载镜像核验版本 2.97、构建 297、bundle ID `com.proletarius.switch`、最低 macOS 27.0、App 和两套后端均为 arm64。
- `codesign --verify --deep --strict` 与 `hdiutil verify` 通过；本地 ad-hoc 签名，未执行 Apple 公证。两套后端对 libusb 使用 `@loader_path`，没有依赖开发机 Homebrew 安装路径。
- 发行包与本地归档 SHA-256 一致，manifest 的大小与摘要一致。

文件：`/Users/proletarius/Desktop/Switch/dist/Switch-2.97-macOS27.dmg`

大小：52,288,943 字节。

SHA-256：`f8c97ce0e294ab76879fabb630f65b05e9ba906fbccd6b954e9d0a1dcf235943`。

验证环境为 Xcode 27.0 (27A266a)、Rust 1.98.0、Go 1.26.7、arm64 macOS 27.2。Go 使用 Switch/build 内临时 shim；Rust 测试使用较短的 Switch/build/tmp 路径，避免独立快照目录导致 Unix socket 路径超限。初次路径失败记录保留，最终以 `2026-10-10-switch-2.97-migration-gates.log` 的通过记录为准。发行核验脚本区分动态库自身的 LC_ID_DYLIB 与真正的加载依赖，最终核验记录为 `2026-10-10-switch-2.97-artifact-verification.log`。

全部日志位于 `/Users/proletarius/Desktop/Switch/build/optimization/2026-10-10-switch-2.97-*`。本轮没有安装/启动 App、操作真实号卡、拨号或发短信。

## 继续推进

DMG 交付后恢复 Iteration 323。整体加权进度仍为 31.4%，目标继续保持 active；本次发行不计迁移分，不代表 Go 已退出。下一步统一读卡器选择和逻辑代次的 Rust 状态机、精确事务回执及旧会话结果隔离；物理丢卡期间的授权证明仍需完整设计与回归，不能通过放宽门禁解决。
