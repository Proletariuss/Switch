# Iteration 276：原生服务生命周期统一owner

日期2026-10-09，Switch_MAC。

NativeSessionServiceOwner组合supervisor、后台恢复Task及脱敏progress订阅。launch使用prepared context的key预留原permit并调用实际spawn_sms_session；reserve或锁失败返回原context供调用者保留租约，不静默丢失启动输入。finish_after_sessions先请求全部停止，检查registry空；非空或锁失败返回owner本身继续清理，空才停止恢复loop并await实际drain。Drop请求停止，不声称同步清理成功。

新增生命周期测试：active slot阻止最终shutdown且permit收到stop；同permit显式完成后owner可以收尾，结果零错误。测试通过，日志../build/optimization/iteration-276-targeted.log。275完整门禁通过，早于本轮生产修改。

该owner仍未接入main公开生产路由；完整真实SIM/IKE/双向短信、API错误等价及跨服务重启恢复仍需验收。整体31.4%，无实卡/发信/DMG。

最新完整门禁通过：../build/optimization/iteration-276-migration-gates.log。
