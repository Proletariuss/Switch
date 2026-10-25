# Iteration 287：daemon退出清理回调驱动可复用

日期2026-10-09，Switch_MAC。

main实际Go terminate等待改为drive_shutdown_callbacks，原两个serve_*_shutdown handlers、16连接上限、biased owner结束优先、listener IO失败及最终abort/drain连接task均保持。helper接受typed owner future与两个handler，供后续原生finish_before并行服务cleanup listeners复用；不新增普通业务准入或扩张shutdown allowlist。

新增双真实Unix listener测试：owner future由oneshot保持未完成，两路cleanup请求分别得到响应，之后owner结束返回原结果。测试通过，日志../build/optimization/iteration-287-targeted.log。286完整门禁通过，早于本轮main修改。

原生owner尚未实际接到main ServerState及该driver，真实Go/Rust读卡释放回调需要组合验收。31.4%，无实卡/发信/DMG。

最新完整门禁通过：../build/optimization/iteration-287-migration-gates.log。
