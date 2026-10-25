# Iteration 293：USB 网络模式路由错误方法收口

2026-10-09，Switch_MAC。

`POST /api/network/usbnet` 已由Rust module-radio owner执行，但ownership表只将POST归Rust，其他方法仍被标为GoCompatibility。Rust的公共方法门禁早已在转发前为该注册路径返回固定 `405 / Allow: POST`，所以这些方法实际不需要Go处理。本轮把整个路径的所有方法归Rust，并核验GET/HEAD/PUT/DELETE均经Rust方法门禁拒绝、仅POST允许。业务命令和数据行为未更改。

定向ownership测试通过；完整迁移门禁已通过（退出码0），日志 ../build/optimization/iteration-293-migration-gates.log。该清理移除一个不可达Go fallback归属，不代表硬件、服务或Go进程已迁移，进度不调整，无实卡操作、短信或DMG。
