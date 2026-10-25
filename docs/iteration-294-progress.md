# Iteration 294：路由所有权与公共方法门禁统一

2026-10-09，Switch_MAC。

`route_owner`现在先检查Rust公共API method gate。某路径已被Rust公开路由注册、请求方法不受支持且门禁明确返回405时，owner判定为Rust；请求在执行Go/Rust业务路由之前就结束。修正了网络服务列表、4G检查、模块飞行模式、health/status及多个eSIM/读卡器路由的错误方法Go归属测试。门禁为Unknown的VoWiFi GET/POST兼容路径仍维持Go归属，未扩大功能接管。

所有权矩阵30项定向测试通过。完整迁移门禁已通过（退出码0），日志 ../build/optimization/iteration-294-migration-gates.log。该项只统一错误方法分派归属，不计Go业务owner退出分；实卡、运营商及发信未触及，31.4%，未生成DMG。
