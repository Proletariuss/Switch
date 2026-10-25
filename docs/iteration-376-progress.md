# Iteration 376：生产 REGISTER 请求线级编码迁入 Rust

生产读卡器 REGISTER TCP transport 现在把有序 URI、SIP 头字段和正文交给 Rust IMS encoder。Rust 生成 start line 和完整 wire frame，校验核心头字段唯一性、拒绝头注入和重复 `Content-Length`，并正确处理二进制正文。Go 保留既有请求语义构造和 socket 写入；生产路径配置 Rust frame probe 却缺少 encoder 时失败关闭。桥接复用现有认证 daemon 操作，不新增 HTTP route。

定向验证通过：Rust `proletarius-ims` 请求编码测试、Rust daemon 编码操作测试、Go `imscore` 生产 transport 测试、Go native IMS bridge 测试。

完整迁移门禁通过：[iteration-376-migration-gates.log](../build/optimization/iteration-376-migration-gates.log)。Rust workspace **1,934 passed / 6 ignored**（daemon **1,383 passed / 3 ignored**）；Go 全包测试与 vet、Swift BLE/manager 与协议 parity、API 契约 `--write-inventory`/`--diff` 及审计器检查通过。最新清单为343 method/path（Rust 283 / Go 60）、127 Swift 调用点、356 审阅范围。

本轮计入 P3 **+0.025 计划人周等值**，累计 **2.140 / 8.0**。全项目为 **16.620 / 47.5 = 34.9895%**，显示 **35.0%**（两位小数34.99%，估值误差约±3个百分点）。Go 仍负责 REGISTER 语义字段组装、socket I/O、物理 AKA、IPsec 安装、事务和 IMS 生命周期；本轮只是请求编码职责接管。DMG 已另行构建并核验：[Switch 2.98 / 构建 298](release-2.98.md)。未进行实机、运营商、短信/通话验收。
