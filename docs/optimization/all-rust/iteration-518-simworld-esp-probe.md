# Iteration 518: SIM World 隧道数据面计数与实卡复测

2026-10-18。本轮只修改 Switch_MAC 当前源码，使用隔离的 `switchd-rs` 数据目录复测已插入 ESTKme-RED 的 SIM World 454/03。已安装的 Switch、号卡 profile 和代理规则没有改动；没有下载 eSIM、发送短信、拨号或生成 DMG。

Rust 隧道增加会话内的有界计数快照：成功提交给选定传输的 ESP 包数、被分类为 ESP 的入站包数，以及入站 ESP 解密或策略拒绝数。计数探针在 CHILD SA 更换时延续；IMS 启动失败后，公开诊断只附加这三个整数，不输出地址、身份、密钥、包内容或逐包时间。现有就绪判定、路由、重试和清理顺序不变。

实卡结果：PC/SC 读取 SIM World 454/03，IKE/EAP 成功并安装隧道，随后 IMS `plain_connect` 在约 32 秒后超时。最终诊断为 `stage=ims_transport, code=transport_failed, io_operation=plain_connect, io_kind=timed_out, esp_sent=8, esp_received=0, esp_rejected=0`。`esp_sent` 只证明本地向既定传输成功提交 8 个 ESP 包；`esp_received=0` 只证明应用在这次会话中没有收到被识别为 ESP 的包。它们不能证明 ePDG 是否收到了包，也无法单独判定代理链路、ePDG 或内层 P-CSCF 目标哪一环丢包。`tunnel_state=connected` 仍不等于 IMS 已注册。隔离会话测试后已停用并正常退出。

回归：新增合成测试覆盖诊断字段的边界和 CHILD SA 更换时计数连续性。完整 Rust 工作区测试为 3027 通过、0 失败、1 忽略，`proletariusd` 构建、格式检查与 `git diff --check` 通过。迁移契约仍为 294/294 Rust HTTP owner、127 个 Swift 调用点、54 个 HEAD alias；只刷新了本轮六处已人工审阅的源码指纹及两处行号。测试记录位于 `build/reports/iteration-518-rust-workspace-final.log` 和 `build/reports/iteration-518-rust-build.log`；运行这些检查不等于运营商 IMS、短信或语音验收。

下一步需要在保留现有 Surge fake-IP/代理路径的前提下，确认当前会话选择的 NAT-T 远端及 P-CSCF 数值目标，并以受控的双端或代理日志区分出站 ESP 是否到达 ePDG；没有这类证据前不修改号卡模板或宣称 SIM World 可用。
