# Iteration 269：恢复候选按尝试顺序轮转

日期2026-10-09，Switch_MAC。

SessionEntry保存last_recovery_attempt，supervisor单调递增领取序号；成功claim才推进，序号耗尽拒绝而非回绕。recovery_candidates先过滤资格，再按上次尝试序号/原generation排序截取：从未尝试优先，同序按generation确定次序，失败解除claim后不会持续霸占批次前列。不使用身份内容排序，不公开key。

新增10记录轮转测试：首批8，失败后下批先选未尝试的最后2，再轮到最早尝试记录；序号耗尽不claim、不破坏恢复可取状态。首轮fixture变量gen与Rust2024保留字冲突，已改generation。12项supervisor测试重跑通过，日志../build/optimization/iteration-269-targeted.log。

仍缺基于时间的退避和后台计时调度，公开startup关闭，整体31.4%，无实卡/外部短信/DMG。

最新完整门禁通过：../build/optimization/iteration-269-migration-gates.log。
