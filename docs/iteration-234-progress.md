# Iteration 234：原生短信启动拒绝语义

日期：2026-10-09。工作区 Switch_MAC。

上一轮组合入口把key不匹配、stop和lease失败统一返回session_mismatch，不能准确保留生命周期原因。本轮提取实际start gate：先核对reader/session key，再核对permit stop，最后核对lease true或watch controller关闭，分别返回native_sms_session_mismatch/native_sms_session_stopped/native_sms_lease_lost。该gate在snapshot与NativeReportOwner创建之前调用，拒绝启动时不预留新的事务session。

新增测试用真实NativeVoWiFiSupervisor permit和watch sender，覆盖正常、错误key、lease true、lease controller关闭，以及stop与lease丢失同时存在时的拒绝优先级。它不是实际注册session/consumer end-to-end证明，不能据此启用生产ready。

完整日志：`../build/optimization/iteration-234-migration-gates.log`：Rust workspace 1662项测试、格式检查、Go全仓test/vet及协议差分（含54组routing）通过，最终 `migration gates passed`。整体31.4%，生产worker-loop与端到端组合仍待完成。无实卡/运营商、实际发信、应用重启或DMG。
