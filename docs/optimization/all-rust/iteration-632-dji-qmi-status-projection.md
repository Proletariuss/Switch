# Iteration 632 — DJI QMI 状态投影与模块页反馈

用户确认故障设备为 DJI 一代 USB/QMI 模块。本轮继续 Iteration 631 的 QMI 启动接线，把 Rust owner 的初始化状态安全地投影到现有模块状态页面，并完成软件构建核验；没有操作实体模块、SIM、通话或短信，也没有生成 DMG。

## 变更

- `/api/status` 增加可选 `qmi` 生命周期字段。`enabled` 表示本次 daemon 以 QMI 模式启动；`session_initialized` 只表示 USB 客户端完成 CTL_SYNC 与 IMS/NAS/UIM 服务客户端初始化。
- Rust 状态投影只读取 owner 缓存，不在页面刷新时额外访问 USB。QMI 初始化失败会显示为启用但未初始化；默认 AT 模式仍不创建 QMI 状态。既有 SIM、运营商、注册与模块在线字段不会被 QMI 初始化状态覆盖。
- 模块概览、菜单栏和流量摘要使用“会话已初始化”表述，并明确当前链路、SIM、网络和实时流量尚未确认，避免把服务客户端初始化误报为模组在线或已有网络流量。
- 首轮 Release 构建发现首页 `ViewBuilder` 内的赋值式条件分支不符合 Swift 视图构建规则，已改为表达式选择文案并重新构建。

## 验证

- Rust QMI 与 QMI 状态投影：40 项通过；状态投影专项：4 项通过。
- `proletarius-api-contract`：9 项通过，1 项忽略（该项要求 Swift handoff 专项脚本）。
- 菜单栏展示：14 个面板渲染、8 种选项组合通过。
- `scripts/preflight-source.sh` 通过；接口盘点为 294 Rust owner、0 Go owner、129 Swift HTTP 调用点。此计数只证明接口归属，不等于产品功能验收。
- Release 应用构建、版本、架构和签名结果见[构建报告](../../../../build/reports/iteration-632-dji-qmi-status-app-build-final.log)与[候选快照](../../../../build/reports/iteration-632-source-snapshot.json)。

## 验收边界

软件测试不能证明这台 DJI 模块在实机上完成 QMI CTL_SYNC、SIM 识别、网络注册、VoWiFi、短信或通话。本轮没有进行硬件验收；若该 QMI 会话仍未初始化，模块页现在会明确区分“已启用但未初始化”，后续应以 daemon 启动日志和实际设备响应定位接口枚举或协议失败。

整体 P0–P6 仍未全部闭合，完整需求分母未冻结，因此不报告综合完成百分比。三轮交叉审议须在全部代码与功能验收闭合后开展；本轮没有生成 DMG。
