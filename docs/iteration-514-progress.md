# Iteration 514：Rust 后端接口阶段包

2026-10-17。后端接口 294/294 归 Rust，整体改造目标保持 active，不能据接口计数标记 100%。

本轮统一历史 CLI 参数解析，恢复 VoWiFi 配置路径 CLI → 环境 → 默认的优先级以及相对路径、按次读取；修复非 UTF-8 路径只在 JSON 显示时转换，文件访问保留原始字节。macOS 不允许创建该无效字节文件名，回归分别验证有效 Unicode 配置实际生效及无效字节路径状态安全序列化。3.43/build343 与旧 3.42 包区分。

完整门禁 Rust 3018通过、0失败、1忽略；Go 退出检查、294路由契约与430个审阅范围、Swift BLE、冻结协议检查通过。2855个冻结输入零漂移。发行包已校验签名、动态依赖、只读挂载及包内 Rust 后端独立启动/退出；未生成或安装 Go 后端。

旧3.41二进制回退夹具在 macOS PTY 串口初始化前失败，不能视为数据兼容已通过。剩余软件缺口包括SIP会话刷新、通话内重协商/媒体更新、CMR驱动编码器，以及旧版回退、Keychain/GUI数据和整机性能验收。12项设备业务与自然租期长稳也未完成；已选ESTK/3HK但本机构建验收没有实体连接结果。9eSIM 6982继续作为已排除问题。

此次应用户明确要求输出[3.43 DMG](/Users/StatTrak/Desktop/Switch_MAC/dist/Switch-3.43-macOS27.dmg)，ad-hoc签名，未公证、未替换安装；无真实卡片变更、对外短信或拨号。

证据：[门禁汇总](../../build/reports/iteration-514-gate-summary.json)、[完整回归](../../build/reports/iteration-514-release-gate.log)、[DMG挂载及包内运行](../../build/reports/iteration-514-dmg-verification.json)、[源码漂移](../../build/reports/iteration-514-source-drift.json)。
