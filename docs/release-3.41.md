# Switch 3.41（构建 341）

2026-10-14。按用户明确要求，以 Iteration 502 已冻结源码构建。

[下载 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.41-macOS27.dmg)。Apple Silicon，macOS 27.0+。

- USB 设备状态、健康、短信设备信息及直接发送路径进一步迁入 Rust。
- 修复设备切换时旧状态残留、短信准备错误分类及号卡身份失效边界；带引号 ICCID 保持显示兼容，内部身份比较准确。保留原短信账本、结果未知禁止自动重放等规则。
- 完整软件门禁通过：Rust 2,799 passed / 0 failed / 7 ignored（4个差分夹具在后段另行通过，3个外部互操作夹具未执行）；Go vet/test、Swift BLE、13组协议差分、49项扫描器自测通过。
- Release 构建、严格签名、DMG校验、真实只读挂载以及包内应用逐文件比较通过。4个 Mach-O 均为 arm64，动态依赖和系统版本要求已核对；13份许可证齐全。构建至最终验证源码无漂移。

本机 ad-hoc 签名，未公证。没有安装替换现有应用；本版未进行实卡、运营商通话/短信或长稳验收。仍包含 Go 后端，全 Rust 目标未完成。

SHA-256：`23f817dffcd13e8cfb81b4dc9dc79c0b122b9a0c1fb35587ed00a06da6aae7a9`

大小：52,220,581 字节。

[本轮源码记录](iteration-502-progress.md)。证据：../build/reports/release-3.41/。验证后仅更新发行文档、README和项目进度记录。
