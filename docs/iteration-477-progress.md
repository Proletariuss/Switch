# Iteration 477：Rust 接管通话模式运行时准备流程，发布 Switch 3.17

本轮将下载完成后的模块侧运行时准备与启停自检合并为一个由 Rust 持有的有界工作流程。新增生产所有权计 P2B **+0.025**，P2B 为 **5.000/8.0（62.50%）**，整体为 **18.170/47.5 = 38.25%（估算误差约 ±3 个百分点）**。计分仅对应新接管的准备流程，不将测试数量、既有私有接口或锁机制重复计入；全 Rust 后端目标仍在实施。

## 生产流程及边界

Go 在运行时下载完成后只发起一次准备 RPC，由 Rust 顺序完成部署、语音路由启动及必要的状态核验、语音路由停止及必要的状态核验、真实音频回滚、最终 boot ID 核验、等待 USB AT `release_confirmed`，最后提交 `prepared`。准备状态绑定当前 producer、Rust 签发的 runtime generation 及模块 boot ID；过期代次、状态矛盾、未确认的清理和模块重启均不能提交成功。

启停响应不确定时，Rust 查询实际路由状态进行核验，不以重复部署或盲目重试解决不确定结果。停止响应不确定而状态已停止时，仍执行真实音频回滚：确认 D4 helper 已退出，再关闭 `audio_enable` 并完成 T/T/B 音频控制，避免仅凭 helper 消失就认定回滚完成。回滚失败保留自检失败阶段和诊断信息。

原生准备流程等待物理 I/O 与 transport close 完成后才确认 USB 租约释放；逻辑租约已经消失或 producer 已退役，也不意味着旧 transport 已关闭。清理错误返回已落定的结果，不再由 Drop 发起第二次未观察的清理。HTTP 请求取消不会取消独立 worker，也不会在实际 ADB 工作及 USB 清理完成前释放真实 guard；`prepared` 提交发生在清理成功之后。

Go 删除原先部署与自检分开的重复编排，保留外层通话模式生命周期、下载、恢复、状态发布、watchdog、硬件配置及普通停止入口。本轮未接管完整通话模式；普通语音路由启停等既有路径仍使用带观察时限的 legacy `release`/Drop 清理，不能将原生准备流程的 `release_confirmed` 语义推及所有调用点。

## 离线验证及审查证据

现有 `build/reports/iteration-477-migration-gates.log` 完整门禁以 `migration gates passed` 结束。按 workspace 测试段汇总，Rust **2,152 passed / 0 failed / 6 ignored**，其中 daemon **1,516 passed / 0 failed / 3 ignored**；后续差分专项复跑不重复计入 workspace 总数。Go 全仓 test/vet、Swift BLE 38 组虚拟事件回归、2,726 项 carrier replay，以及 USB/radio/HTTP/HEAD/IMS/SDP/AMR/MESSAGE/USSD 差分均通过。

Rust ADB 专项 **47 项通过**；USB AT **78 项通过**，包括本轮新增的 5 项确认释放回归，覆盖阻塞 close、逻辑租约提前消失、退役后的旧 close、取消等待以及明确失败后不重复触发 Drop 清理。Go 通话模式定向 race 测试 **18 个顶层用例通过**，覆盖单 RPC 准备、失败阶段及状态兼容、代次保留和不确定变更不重试。

契约 `--diff` 通过：**354** 个 method/path（Rust **295** / Go **59**，含 **51** 个 HEAD alias）、**127** 个 Swift HTTP 调用点及 **381** 个审阅范围。Go 结构退出审计仍有 **6 类阻塞**、**265 个生产 Go 文件**；Rust gateway 仍依赖 Go 后端，App 仍构建并打包 Go 二进制。

证据均位于 Switch 工作区：`build/reports/iteration-477-migration-gates.log`、`iteration-477-call-mode-adb-tests.log`、`iteration-477-go-call-mode-tests.log`、`iteration-477-go-call-mode-race-tests.log`、`iteration-477-contract-inventory.log`、`iteration-477-contract-diff.log` 和 `iteration-477-go-exit-audit.log`。上述结果来自已有日志，本记录编写期间未重新运行测试、构建或硬件操作。

未操作真实读卡器或号卡、未发送短信或拨号、未做设备和运营商网络验收。静态及离线证据不能代替实机验证；9eSIM `6982` 继续排除在本轮范围之外。

## Switch 3.17 发行校验：通过

按用户要求已生成 [Switch 3.17 DMG](/Users/proletarius/Desktop/Switch/dist/Switch-3.17-macOS27.dmg)，build 317，arm64，最低 macOS 27.0。大小为 54,045,359 字节，SHA-256：`e3c3c2d272ebb4bc9c48ba58f03fb37016b1dac01bdd6079d97dc629a0f20573`。

此安装包包含 Iteration 477 的最新生产源码。版本文件、Xcode 设置与 Rust workspace 均同步为 3.17 / build 317 / Rust 3.17.0，外部依赖锁定版本未改变。scripts/release.sh 从当前源码重新构建 Swift、Go 和 Rust，Release 构建与签名通过。旧 3.16 DMG 保留，仅覆盖 Iteration 476。

磁盘映像校验通过；只读挂载后核对应用版本/build、最低系统版本、Applications 链接、11 项许可文件、App/两后端/libusb 的 arm64 架构及严格签名。包内 libusb 链接使用 loader_path，不引用开发机目录。主发行文件、本地归档和发行清单的大小及 SHA-256 一致；dist/Switch.app 已用核验后的同一包内应用同步，四个二进制哈希均一致。验证挂载已卸载，没有启动 App 或进行硬件测试。

本机 ad-hoc 签名，未进行 Developer ID 公证。本次打包不代表真机、运营商网络、短信或通话质量的新增验收。全 Rust 迁移目标保持进行中，进度仍为38.25%。

发行证据：build/reports/release-3.17-version.log、release-3.17-build.log、release-3.17-verify.log、release-3.17-verification.json；发行清单位于 dist/release-manifest.json。所有路径均相对 Switch 根目录。
