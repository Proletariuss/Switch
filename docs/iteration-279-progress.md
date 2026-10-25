# Iteration 279：原生owner两阶段关闭边界

日期2026-10-09，Switch_MAC。

审阅main shutdown：先module/reader begin_shutdown及abort入口任务，再Go terminate期间接受有限cleanup回调，最后关闭物理transport。原生session lease仍依赖兼容桥，因此main接管前必须证明原生IMS/network/lease清理完成之前保留所需回调与Go server；当前不直接改动已有正常退出顺序。

NativeSessionServiceOwner新增非消耗begin_shutdown：置stopping、请求同代全部停止，但保留recovery Task/订阅。finish_after_sessions复用begin，再检查registry空、停止recovery并await drain。为main准备先quiesce后保留桥清理的接口，不将接口存在等同生产接管。

新增实际owner准入/permit测试证明begin立即拒绝新准入、stop已传播、recovery仍持有、重复begin幂等、旧slot完成后可finish。37项session模块测试通过，日志../build/optimization/iteration-279-targeted.log。278完整门禁通过，早于本轮修改。

main生命周期实际接入及回调依赖证明仍缺；31.4%，无实卡/外部发信/DMG。
