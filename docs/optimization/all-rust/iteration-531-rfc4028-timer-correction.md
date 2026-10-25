# Iteration 531：校正 IMS 会话定时器的成功刷新语义

2026-10-18。按 [RFC 4028 §7.2](https://www.rfc-editor.org/rfc/rfc4028.html) 复核上一轮的 SIP 会话定时器：对端对刷新 UPDATE 返回不含 `Session-Expires` 的 2xx 时，会话可以继续，但该对话不再有会话到期时间。原实现错误地发 BYE；已改为清除定时器并保留通话。入站 UPDATE 或 re-INVITE 产生不含该头的 200 后也清除旧期限，避免继续按过期计时器挂断。

同时补齐事务边界：带 `Require: timer` 却缺少 `Session-Expires` 的入站 INVITE 在响铃前返回 400；已接听 INVITE 的同事务重传 200 保留首次应答的 `Session-Expires`、`Require: timer` 和 SDP。依据 [RFC 4028 §9](https://www.rfc-editor.org/rfc/rfc4028.html)，服务器不能在 200 中自行提高请求的间隔；低于 `Min-SE` 的入站 INVITE、UPDATE 和 re-INVITE 现在返回带该下限的 422，同事务 re-INVITE 重传沿用原响应。测试覆盖有效的定时器关闭、矛盾头部的拒绝、422 下限、重传应答、普通 UPDATE 和 re-INVITE 清除期限。主叫初始 INVITE 收到 422 后的重试及对不支持 UPDATE 的对端改用 re-INVITE 刷新仍是后续缺口。

完整 Rust 工作区回归 **3053 通过、0 失败、1 忽略**；`switchd-rs` 开发构建、格式检查、`git diff --check` 和公开接口盘点通过，仍为 **294/294** Rust HTTP owner。日志分别在 `src/build/reports/iteration-531-final-workspace.log`、`iteration-531-daemon-build.log` 和 `iteration-531-contracts.log`。

使用本轮源码构建的隔离 Rust daemon 再次连接 ESTKme-RED 中的 SIM World。隧道进入 `connected`，IMS 进入 `registering`，随后 `ims_transport / transport_failed`、`plain_connect / timed_out`；ESP 发出 **8**、收到 **0**、拒绝 **0**，通话与短信均未就绪。Surge 对这次进程的会话记录显示选择“香港 14 家宽”，整体出站 2051 字节、入站 1188 字节且 `failed=false`；入站总量包含 IKE 回复，不能解释为 ESP 已有回包，也不能证明 ePDG 收到了出站 ESP。脱敏结果在 `src/build/reports/iteration-531-simworld-sanitized.json`。测试结束后明确停止 VoWiFi、断开读卡器并退出隔离 daemon；隔离运行目录已清理。当前证据仍无法区分代理出口、ePDG 接收或内层 P-CSCF 路径的丢包位置，运营商业务验收未完成。

本轮未修改代理规则、切换 profile、发送短信、拨号或生成 DMG。
