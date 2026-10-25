# Iteration 225：原生报告会话容量与退出责任

日期：2026-10-09。工作区 Switch_MAC。

NativeReportOwner 创建时在共享Service中预留会话与单调owner token；重用已有session_id拒绝，遵守256会话容量。native claim存活时旧retire入口返回conflict，不能删除正在使用的会话。Drop只清理仍匹配本token的会话：无outgoing batch时删除释放容量；有未完成/未收尾发送batch时仅解除claim，保留事务恢复状态，不删除或自动重发。被替代token不会被旧析构清除。

consumer在durable archive完成后再次核对活动状态，再调用共享报告准备，避免归档等待期间stop已到达后继续分配报告。没有把析构本地事务状态等同于网络actor已经退出；真正IMS/network cleanup仍由已有native actor任务负责。

新增测试：连续创建/退出300次不耗尽容量，重复claim及旧retire被拒绝；模拟延迟旧析构不删除替代owner，准备中的发送batch在新owner退出后仍保留且不会被新claim覆盖。既有consumer归档/停止/未知写入4项定向测试通过。

完整门禁：`../build/optimization/iteration-225-migration-gates.log`：Rust workspace 1656项测试、格式检查、Go全仓test/vet与既有协议差分通过，最终标记 `migration gates passed`。生产完整原生启动、注册routing snapshot、持久report恢复及loop/task收尾组合仍待完成；正常旧session行为不变，只有原生claim拒绝旧retire。整体31.4%，无设备变更、实际发信、应用重启或DMG。
