> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# 迭代 044：reader lease 到 Rust IMS 的 ISIM 身份接缝

- 版本：2.23 / build 223（2026-09-25）
- 目标：避免 Rust IMS 注册在 carrier override 选择 `isim/auto` 时因 lease 丢失 ISIM 身份而拒绝；保证身份只通过已认证的私有 reader lease 传递。
- 实现：Go reader lease 从当前 generation 对应的 USIM/ISIM cache 携带 IMPI/IMPU；Rust lease 在反序列化、校验与会话持有期间使用 `Zeroizing<String>`。Rust IMS profile 支持 `derived/isim/auto`，验证成对身份、scheme、host 和安全字符，并将 IMPU 用于 REGISTER identity、IMPI 用于 AKA Digest username。ISIM 身份不会进入公开快照或日志。
- 验证：`cargo +1.98.0 fmt --all -- --check`；`cargo +1.98.0 test -p proletarius-vowifi --lib`（132 passed）；`cargo +1.98.0 test -p proletariusd`（160 passed）；Go reader lease/API 定向测试通过。Release daemon 与正式 macOS 27 ARM64 DMG 通过后记录具体镜像摘要。
- 未完成/验收边界：当前 Rust keyed session actor 和 public start route 尚未接通；lease 保存的 ISIM 值暂未被 live IMS owner 消费，本轮不切换生产 Go owner。无实体 SIM、真实 ePDG/IMS、SMS、主被叫通话测试。生产 Rust 所有权仍 9/15（60%）。
- 取舍：只扩展既有私有 lease 与 REGISTER profile，不增第三方依赖或公开接口，不改代理、DNS、UI 和既有 Go 生产路径。准备后续切换所需的敏感输入，但不把代码可用误报为生产可用。
- 产物：`dist/Proletarius-2.23-macOS27.dmg`，42,336,228 字节，SHA-256 `9149b6c5c261fed1dd51bac6cb84f3b1546f7de948cb839fb3e2aabe4c8d69c7`；macOS 27 ARM64，签名检查与 `hdiutil verify` 通过，并复制到桌面 `test` 文件夹。未使用 UPX。
