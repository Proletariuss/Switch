> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# 迭代 046：IMS 注册复用 session 绑定身份

- 版本：2.25 / build 225（2026-09-25）
- 基线：2.24 已在 native preparation 中从 reader lease 解析并持有零化 IMPI/IMPU，但 REGISTER coordinator 尚未消费这份 session 身份，仍可能在其内部再次根据运行时 SIM 字段解析。
- 实施：subscriber identity 增加私有绑定元数据（carrier、IMS domain/realm、identity source、零化 IMSI）；加入 session-bound REGISTER 入口。profile/IMSI 不匹配时在网络连接前返回类型化错误；同一份 resolved identity 用于初始、AKA 后以及 sec-agree protected REGISTER。原兼容入口保留。
- 验证：VoWiFi 133 项测试、daemon 161 项测试通过；动态 sec-agree loopback 覆盖同一绑定身份贯穿 plain→AKA→protected REGISTER，单元用例覆盖跨 IMSI/carrier 错配。
- 收益/成本判断：小型身份快照元数据和窄适配器解决“准备期身份”和“注册期身份”可能不一致的问题；复用已有 coordinator 和安全会话实现，无新依赖、不增加 DNS 路径，也不改 Go 通信 owner。
- 边界：尚未将 gated native session actor 接入生产启动路由；没有连接真实卡/ePDG/IMS，也没有短信、通话或来电验收。生产 ownership 仍为 9/15（60%），该值表示 Rust 生产组件所有权而非总体项目进度。
- 发行：macOS 27.0 ARM64 Release 构建成功；App ad-hoc 签名 `codesign --verify --deep --strict` 与 `hdiutil verify` 均通过。DMG 为 42,337,521 字节，SHA-256：`7e0cf88c4e4973095bb766e377d8e674a327b46308ffcfdadf1a3a90040c2771`。产物位于 `dist/`，并复制到桌面 `test/`；两份摘要一致。不使用 UPX。
