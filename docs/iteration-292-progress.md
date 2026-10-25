# Iteration 292：生命周期锁中毒后的安全恢复

2026-10-09，Switch_MAC。

NativeLifecycle之前一旦Mutex poison就永远返回unavailable，导致Go桥与callback pump无限等待，即使owner已完成清理也无法收尾。本轮通过`into_inner`恢复状态锁，在owner仍持有时重申stopping并调用幂等begin_shutdown；owner清理任务回写后先恢复`finishing=false`、`stopping=true`与owner/失败锁存，再清除poison。若等待期限到期，原owner及supervisor slot保留，返回unconfirmed；实测解除slot后第二次关闭成功。外层关闭任务JoinError仍写入失败锁存，禁止误判empty success。

专项poison测试通过（200ms超时、slot和recovery保留、完成slot后重新关闭成功）；日志 ../build/optimization/iteration-292-targeted.log。完整迁移门禁已通过（退出码0）：../build/optimization/iteration-292-migration-gates.log。

此次只修复退出可靠性，不改变生产readiness、不计接管分。完整VoWiFi闭环、业务接管、Go退出和实机验收仍缺；进度31.4%，未生成DMG。
