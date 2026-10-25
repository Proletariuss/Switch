# Iteration 480：原生模块通话控制与阶段发行

本轮由 Rust 接管普通模块蜂窝通话的状态查询、拨号、接听、挂断和 DTMF。串口与 USB 的 CLCC 查询、来电和通话事件、持续通话 radio reservation、状态修订号以及有序通话记录均使用原生 owner。Go 中 5 条私有 CS 适配路由、4 个旧公开注册、旧通话状态、radio worker 和 CS 通话记录生产者已退役。模块 IMS 与读卡器 IMS 仍通过既有兼容层工作，不能将本轮解释为所有语音或全 Rust 后端已经完成。

## 行为与会话边界

保持既有公开 JSON 和显示语义；held、号码缺失或格式异常的 CLCC 行即使兼容显示为 idle，也不能据此授权新拨号、释放保护或结束通话档案。近期 RING 仅在同一会话的 10 秒窗口内用于来电判断。晚到的 CLCC、ATA 或 ATH 回复不能覆盖新来电；ATD 回复前已观察到 CONNECT 时，记录保留实际事件时间与开始顺序。发送结果不确定时只查询状态，不重放动作。

实际串口/USB 写入前再次核对 lease、producer、物理 session 和业务准入。USB drain 中观察到新来电会撤销过期动作。事件在同一物理 owner 中捕获，覆盖 drain、命令响应和 idle；首个 RING 在 binding 提交窗口内有界保留，旧 session 事件不能挂到新设备。未确认的事件保留原队首和原时间重试，溢出或失败公开为 history_error。CS 查询和只读探测不会覆盖既有 CFUN/RF 事实。

HTTP 观察者取消后，已接受任务仍持有实际资源，直到真正完成。请求数量有界，控制操作等待物理通道也有期限；释放未确认时保留原 token 并拒绝新动作复用。Go 进程替换不结束原生 CS 档案；只有旧 transport 确认关闭后才恢复准入，同时撤销旧 RING/cache。最终退出等待真实任务、radio 清理与归档 worker 排空后才继续关闭底层连接。

## 记录与错误兼容

CS 使用独立 producer 和有界单写入队列；归档不在状态锁内执行。不可变待确认记录按原顺序重试，退休记录有预留容量。暂时存储错误只在同一记录确实写入并得到确认后清除。Go producer 替换仅作用于剩余 IMS 来源。新增受认证、大小限制与当前 producer 约束的 IMS history_error 通道，普通通话和读卡器通话状态都保留合并后的归档错误。

## 验证与发行

完整迁移门禁通过；最终退出顺序补强后重新执行 Rust workspace：**2,237 passed / 0 failed / 6 ignored**，其中 daemon **1,601 passed / 0 failed / 3 ignored**。Go build、全仓 test/vet 和定向 race 通过；Swift BLE 38 组、2,726 项 carrier replay，以及 USB/radio/HTTP/HEAD/IMS/SDP/AMR/MESSAGE/USSD 差分通过。契约工具 45 项与最终普通契约检查通过。专项测试与差分不重复计入 workspace 总数。

契约盘点：347 个 method/path（Rust 300、Go 47），51 个 HEAD alias、127 个 Swift 调用点、384 个审阅范围。Go owner/dependency 路由降至 157，生产 Go 文件 257；仍有 6 类结构退出阻塞。

打包前交叉审议修正了旧 RING 在 transport 更换后的准入、先关闭旧 serial session 再恢复业务、初次绑定查询的 revision 误失效，以及实际 drain 未确认就关闭下层 transport 的顺序问题。中间编译期 fixture 接口不一致已修正，最终专项和完整门禁均通过。构建阶段固定到 3.20/build 320；Cargo.lock 仅更新 workspace 版本，没有改变第三方依赖版本。

验证日志：`build/reports/iteration-480-migration-gates.log`、`iteration-480-final-rust-workspace.log`、`iteration-480-module-call-tests.log`；Go、race、契约与投影结果位于 `build/iteration480/`。3.20 DMG 已完成下述发行校验。没有运行实机、发送短信或拨打电话；9eSIM 6982 不在本轮处理范围。

全 Rust 最终目标仍在进行。前次已审计估值为 18.195/47.5（38.31%）；本次发行不通过路由数、测试数或文件数追加分数，后续按完整能力验收核算。

## 已交付安装包

按用户要求，已生成并验证 [Switch 3.20 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.20-macOS27.dmg)，build 320，源码截止 Iteration 480。大小 **53,364,565 字节**，SHA-256：`1e06fd8970cf283bd8e605130b310454a04aabe37c6be07e82f794f4d8b76448`。

Release 构建、App 与全部嵌套 Mach-O 严格签名、arm64 架构与最低 macOS 27.0、13 份许可文件、实际加载依赖无 Homebrew 路径、镜像完整性、只读挂载和归档 manifest 哈希校验均通过。`dist/Switch.app` 与镜像一致，镜像已卸载。构建前后 **4,094 个发行源码文件**的哈希不变（包含使用的第三方源码；记录不是完整源码归档）。

发行记录位于 `build/reports/release-3.20-build.log`、`release-3.20-verify.log`、`release-3.20-verification.json` 和 `release-3.20-source-files.json`。本机 ad-hoc 签名，未进行 Developer ID 公证或实机验收；仍包含 Go 后端。
