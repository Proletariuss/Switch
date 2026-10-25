# Iteration 098：PC/SC 统一实体会话与并发缺口修复

日期：2026-10-06。用户要求修复缺口后交付 DMG，本轮版本为 2.81 / build 281。全 Rust 后端目标保持执行中；本轮没有访问实体卡、发送短信、呼叫或读取生产数据。

## 行为与边界

Rust `reader_pcsc` 统一持有 PC/SC 物理句柄、完整事务、租约、APDU、复位和真实关闭责任。Go PC/SC 卡片通道改为固定 session/lease 的代理；Rust LPA、身份读取、AKA 和原生 VoWiFi 鉴权使用同一 owner。Go 原 `reader_ccid_persistent.go` 与直接 PC/SC 打开/复位路径移除；设备枚举、业务排序/最终化、缓存、BLE、IMS 和 N 待仍有 Go 依赖。

新增 8 个仅内部凭证可调用的 POST 动作。生产者、会话、租约及单次 APDU 都有明确身份与序号；ACK 丢失不得重放 APDU。generic CCID 复用句柄，ESTKme 保留原复位与关闭策略，完整原生闭包退出前不释放实体占用。真实 I/O 未返回、关闭失败或 worker 退出未确认时保留收尾责任，不因 HTTP 超时假定已释放。

交叉审阅后修复：

- **长租约误借用**：删除全局 `pcscBorrow`。Go 最终化回调整个期间持有业务锁，并显式传递可撤销的 context 能力。普通刷新不能在等待期间进入同一租约；退出后的 context/通道失效。BLE 保留独立既有行为。
- **迟到 open 复活**：确定性门闩先复现“close 已 ACK，等待旧句柄关闭的 open 又成功发布”。最终发布在同一 owner 锁下重新核对关闭序号，并复核期间新租约、原生借用及物理句柄状态。
- **旧 close 清新会话**：关闭的等待和清理仅针对原 session ID/sequence。替换会话不能被迟到的收尾清除。
- **旧 producer 关闭污染新序号**：producer 校验、关闭序号记录及 worker 选择合并为同一临界区；真实换代后旧 close 不能阻止新 producer 从序号 1 打开。
- **公开错误兼容**：读卡器不存在和 `NoReadersAvailable` 保持 `reader_unavailable / HTTP 409`；不确定提交状态保留。

详见 [PC/SC 契约](optimization/all-rust/pcsc-owner-contract.md)。

## 验证证据

完整 `scripts/test-migration-gates.sh` exit 0：Rust workspace **1133 通过、0 失败、14 个默认忽略**；Go vet / 全包兼容测试通过，运营商、USB 状态、RF、HTTP 方法、SDP/AMR 差分通过。14 个默认忽略不是全部已验收；门禁单独执行其中的 USB/RF/HTTP 差分。PCMA 仅为历史离线夹具，不作为通话选项。两个既有 native VoWiFi dead-code warning 保留。

Go 最终 `go test -race -count=1 -json ./cmd/proletarius-macos`：**546 个顶层 / 1266 个含子测试通过，0 失败、2 个默认跳过**。跳过项为独立多进程夹具和需额外 Rust 输入的 SMS 提交差分；本轮不据此宣称这两项已重跑。确定性回归覆盖等待期间的普通刷新、真实最终化路径、撤销后的 context/通道拒绝，以及固定 ESTK 会话复位边界。

日志位于 `Switch/build/optimization/`：

- `2026-10-06-pcsc-publication-red.log`：先复现迟到发布缺口。
- `2026-10-06-pcsc-migration-gates.log`：完整离线门禁。
- `2026-10-06-reader-pcsc-callback-fullcmd-race.jsonl`：Go 最终并发回归。

最后修复 producer 关闭竞争后，Rust `reader_` 定向回归 **120 通过、0 失败、0 忽略**，包含 owner、LPA、身份/AKA、租约及真实 UDS 权限/退出路由；`cargo fmt --check` 与 `git diff --check` 通过。日志：`2026-10-06-pcsc-final-reader-tests.log`。契约清单已生成 **248 条（Rust 150、Go 98）**，20 条为 HEAD 别名，独立 Rust 实现 130 条；Swift 调用点 127、审阅 scope 185。8 条新增私有 PC/SC 契约明确保留 Go 调用者依赖。工具 41 项自测通过；最终一致性检查 exit 0；Rust-only 按预期 exit 1，仅因默认 Go fallback 与 159 条 Go 归属/依赖契约，未出现指纹漂移。17 份仅测试引用的 Rust 源码按显式引用图排除。证据日志：`2026-10-06-pcsc-contract-{write,check,selftests,rust-only,review-refresh}.log`。

## 进度与剩余工作

原 47.5 人周基线的已验收估算暂维持 **24.9%（11.825 / 47.5，误差约 ±3 个百分点）**；本轮增加实体 owner 接管证据，尚未完成实体卡/长期运行及完整 reader 业务退出，不按私有路由数提高总百分比。EVS 原生能力新增范围仍待试点重估。当前 Rust + Go 双后端仍是迁移阶段产物，不能标为零 Go 或 100%。

后续重点：完成完整下载/通知的原生单事务计数证据、BLE 与剩余卡片业务；继续 IMS/VoWiFi/N 待真实会话、媒体和数据兼容接管。实机插拔、固件、Profile 切换及运营商长期稳定性单独验收。语音目标 EVS 优先、AMR 回退、不用 PCMA；当前仍为 AMR-NB mode 7，EVS/AMR-WB 未实现。9eSIM `6982` 继续排除。

## 2.81 阶段包交付

`Switch/dist/Switch-2.81-arm64.dmg`，**49,977,297 字节**（约 50 MB），Apple Silicon，最低 macOS 27.0。未覆盖已有 2.79/2.80 DMG。

Release `scripts/build.sh` exit 0，Swift App、Go 兼容后端、Rust 后端与内置 libusb 均为 arm64。构建过程只更新本地包版本为 2.81.0，未重新解析或升级第三方依赖。App 为既有 ad-hoc 签名方式，不是已公证发行包。

`hdiutil verify` 通过；只读挂载后再次验证包内版本、App 严格签名、架构、libusb 相对加载路径、图标/许可及 Applications 快捷方式。包内 26 个文件/链接逐项比对已签名构建产物，完全一致；校验后已卸载镜像。没有启动 App、连接卡片或进行真实运营商测试。

SHA-256：`050f914319d51fc7f65046507bc692671c37f598fadfaf8489668ac4cc47d96f`。

证据：`2026-10-06-switch-2.81-release-build.log`、`2026-10-06-switch-2.81-dmg-package.log`、`2026-10-06-switch-2.81-package-verification.json`。摘要 `2026-10-06-pcsc-results.json`。本包包含 Go 兼容后端，全 Rust 目标继续执行。
