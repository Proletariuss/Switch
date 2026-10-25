# Iteration 429：IMS 入站短信归档确认与 OPTIONS 心跳策略收敛

日期：2026-10-12。全量离线迁移门禁通过，API 契约扫描差异复核通过；349 条 method/path（Rust 290 / Go 59）、127 个 Swift HTTP 调用点、371 个审阅范围。门禁日志见 [iteration429-final-gates.log](../build/optimization/iteration429-final-gates.log)，Go 退出快照见 [iteration429-go-exit.txt](../build/optimization/iteration429-go-exit.txt)。

普通 IMS 入站短信不再配置 Go 归档回调或 Go 持久化回退。消息解码与耐久归档须由 Rust coordinator 确认；只有 store_state=archived 才发送 RP-ACK，解码失败或缺少归档确认时保留重试机会并暂不确认。N待原始短信仍经独立的 Profile 级存储/观察回调后再确认，状态报告保持单独处理。呼出短信提交结果归档仍由现有 Go 兼容路径完成。

OPTIONS 心跳请求构造、响应 SIP 状态/结束帧观察及会话探测的继续、复核、恢复和终止决策由 Rust 接口提供。Go 仍负责受保护 SIP socket、等待与计时器执行、readiness 副作用和最终传输清理。

这轮删除了普通短信归档的冗余 Go fallback，但该 fallback 在生产常规路径中已由 Rust coordinator 完成耐久写入，因此不计新增加权迁移分。全项目进度维持 **17.870 / 47.5 = 37.62%**（估算误差约 ±3 个百分点）。结构退出门禁仍有 6 类 Go 依赖和 264 个生产 Go 文件；没有读卡器、实体 SIM、短信发送或运营商网络实机验收。最终门禁日志 SHA-256：b4c4db76e4770e2e3998eaec8801abaf51012896336da3d3c89ea9145ff1bc0b。

## DMG 构建

按用户要求生成 Switch 3.11/build311 Release DMG。Xcode Release 构建成功，DMG 可挂载且 hdiutil 校验通过；包内 CFBundle 版本为 3.11（311），arm64 app 的代码签名验证通过。当前签名为本机 ad-hoc 签名，未提交 Apple 公证。产物：[/Users/proletarius/Desktop/Switch/dist/Switch-3.11-macOS27.dmg](/Users/proletarius/Desktop/Switch/dist/Switch-3.11-macOS27.dmg)，SHA-256：674bc4c2c90418bbd233e9d6ff1d9b183da64566e28e00dd8944ded4e897b777；大小约 51 MiB。
