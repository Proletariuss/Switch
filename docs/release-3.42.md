# Switch 3.42（构建 342）

2026-10-16。按用户明确要求，以 Iteration 503 已冻结源码构建。

[下载 DMG](/Users/StatTrak/Desktop/Switch_MAC/dist/Switch-3.42-macOS27.dmg)。Apple Silicon，macOS 27.0+。

- Rust 接管独立 USB 启动和短信就绪，Go 兼容进程退出不再连带撤销原生 USB 会话；保留串口、读卡器及 QMI 的既有兼容功能。
- 修复设备未就绪时本机短信归档数量不可见，以及设备、配置代次切换后的旧观察隔离边界。
- Rust 2,831/0/7、Go vet/test、Swift BLE、13组协议差分、49项扫描器自测通过；4个ignored差分夹具已另行执行，3个转发跨语言互操作夹具本轮未执行。完整阶段证据见 `gate-components.json`。
- Release、严格签名、镜像校验及真实只读挂载通过；包内应用与签名构建逐文件一致，动态依赖和许可证核查通过，冻结源码没有漂移。

本机 ad-hoc 签名，未公证。没有安装替换现有应用；本版未进行实卡、运营商通话/短信或长稳验收。仍包含 Go 后端，全 Rust 目标未完成。

SHA-256：`fdd27ca1fa9e6144ab240fdea4ec2660f92b7af057e35c801d8af31850e13f63`

大小：52,268,912 字节。

[本轮源码记录](iteration-503-progress.md)。证据：`../build/reports/release-3.42/`。发布验证后只更新文档。
