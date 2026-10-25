# Iteration 666：Swift / Rust 最终责任边界

日期：2026-10-22（Asia/Shanghai）。本记录解释“后端全部使用 Rust”的可审计口径，并固定本轮剩余审查范围。

## Rust 必须持有

- 硬件发现后的选择、租约、代次、恢复、重试、冷却、超时和终态判定；
- eSIM、短信、VoWiFi、IMS、呼叫、网络恢复及转发队列的业务状态机；
- 写入前准入、first-committed-wins、未知提交结果、崩溃恢复和耐久提交；
- 认证、输入验证、隐私脱敏、代理/运营商策略及所有公开后端接口；
- 会影响“是否执行、执行哪一个、何时重试、是否成功”的决定。

Swift 只能持有 macOS/UI 适配：SwiftUI 状态绑定、CoreBluetooth/CoreAudio 回调、通知与窗口、Keychain/UserDefaults 原始字节读取、HTTP/FFI 字节搬运，以及用户明确选择的展示偏好。Swift 收到的迟到响应必须受 Rust 提供的 operation / generation 约束，不得重新推导业务成功。

## 软件更新检查的边界决定

`UpdateChecker.swift` 处理当前 App 的版本文字、用户选择的 stable/beta 渠道、当前 CPU/系统版本、系统浏览器打开和弹窗状态。它没有访问 SIM、模块、读卡器、VoWiFi、短信或 Rust daemon 的业务资源，也不作为任何通信后端调用的回退实现。因此保留为前端发行适配，不计入“Rust 后端”迁移项。

该例外只覆盖发布列表的展示与当前 Mac 的 DMG 资产选择。若以后增加自动下载、校验、安装、回滚、跨进程更新锁或持久任务恢复，这些可改变安装状态的流程必须先建 Rust owner，再由 Swift 调用，不能沿用本例外扩大范围。

## 最终扫描判定

最终冻结前，对 Swift 源码执行业务动词、计时器、重试、循环轮询、URLSession、持久文件和硬件回调扫描。每个命中必须满足以下之一：

1. 已由 Rust 结构化 intent / outcome 驱动，Swift 仅适配；
2. 属于上述 macOS/UI 责任且有明确边界测试；
3. 被记录为阻断项并在冷构建前修复。

不能以“公开 HTTP 路由已在 Rust”代替该扫描；接口覆盖率和业务所有权是两项证据。
