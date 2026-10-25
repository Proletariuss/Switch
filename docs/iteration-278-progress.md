# Iteration 278：服务关闭与后台真实Unix IPC重试闭环

日期2026-10-09，Switch_MAC。

共用恢复fixture改为明确enum模式，新增ServiceShutdown：实际NativeSessionServiceOwner持有registry/recovery/progress，登记原Unix IPC取得的lease并受控确认清理；首次finish因原slot未释放返回owner、拒绝新准入，后台false释放失败后按退避自动再试，同一lease_id得到true才完成原slot，第二次finish成功。最终progress revision2、failed1、confirmed1、原generation、Confirmed均核对。

36项session测试通过，日志../build/optimization/iteration-278-targeted.log。完整门禁../build/optimization/iteration-278-migration-gates.log通过。服务端是fixture、清理证明受控，不声称真实IKE/运营商/读卡释放验收。

公开main owner尚未接入，跨服务持久恢复及原生完整业务验收未完成，31.4%，无实卡/外部发信/DMG。
