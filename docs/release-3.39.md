# Switch 3.39（构建 339）

2026-10-14。按用户要求，从 Iteration 500 当前源码构建。

[下载 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.39-macOS27.dmg)。Apple Silicon，macOS 27.0+。

- 模块 VoWiFi 启停、状态、操作结果、共享 IMS 通话/USSD 和会话列表由 Rust 原生服务接管。
- 切卡与下载先检查通话并等待真实网络/射频清理；补齐精确 USB 关闭证明、Go 退出期间新启用保护，修复原生模块状态受旧后端恢复等待影响的问题。
- 完整门禁：Rust 2,771通过/0失败/7忽略，4个差分忽略项在后段显式通过；Go vet/test、Swift BLE、13组差分、49项扫描器自测及317条契约检查通过。
- Release、严格签名、镜像校验、只读挂载逐文件比较与独立发行审阅通过；4个Mach-O均arm64，13份许可证齐全。两个后端通过不初始化硬件的装载检查。

本机 ad-hoc 签名，未公证。未安装替换，也未开展本版实卡、运营商、真人音频及长稳验收。仍包含 Go 后端，全 Rust 目标未完成。

SHA-256：`0e4ca1b1cd392fa86ac43040b1d6d36d42610bd3eb40128d07466f93a0526569`

大小：53,654,230 字节。

[本轮源码记录](iteration-500-progress.md)。证据：../build/reports/release-3.39/。构建后仅契约元数据、已单独审阅的 cfg(test) 同步修复及发行文档变化，生产编译输入未变。
