> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# 迭代 043：运营商 profile 接入 IMS REGISTER 协调器

- 版本：2.22 / build 222（2026-09-25）
- 目标：把已经解析的原生 carrier profile 接入 Rust IMS REGISTER 协调器，贯通初始 REGISTER、SIM AKA challenge 后的 plain authenticated REGISTER、sec-agree 后的 protected REGISTER；保持原有生产 Go owner 与外部 API 不变。
- 实现：新增通用 `RegisterRequestBuilder` 接口和 `NativeProfileRequestBuilder`，由 profile compiler 生成每种 REGISTER 请求；新增 `register_native_profile` 适配入口，SIM AKA 仍由调用方注入。配置编译错误被单独保留，不混同于网络、鉴权或清理错误。
- 回归：动态 loopback 测试加载 carrier registry 中 Orange UK/CTExcel profile，经过两个 userspace netstack 验证 plain REGISTER → 模拟 AKA → sec-agree → protected REGISTER；还验证同一受保护关联上的合并 INVITE、应答路由以及 SA 和端口清理/复用。
- 验证通过：`cargo +1.98.0 fmt --all -- --check`；`proletarius-vowifi` 129 项测试；`proletariusd` 160 项测试；`cargo +1.98.0 build --release -p proletariusd`；正式 `scripts/release.sh` 的 macOS 27 ARM64 Xcode Release 构建、ad-hoc 签名严格校验与 `hdiutil verify`。
- DMG：`dist/Proletarius-2.22-macOS27.dmg`，42,334,590 字节，SHA-256 `4c3d9dece6d87a90376407bb6bb34f2fcf49b817ccf4c0206970a943617c97b1`；同时复制到桌面 `test/Proletarius-2.22-macOS27.dmg`。未使用 UPX。
- 验收边界：loopback 采用模拟 SIM AKA 与受控 ePDG/IMS，对本机协议组件接线提供证据；没有真实 ePDG、实体 SIM、运营商注册、短信或通话测试。未接入 production keyed session actor/public start route，没有关闭 Go 通信 owner。生产归属维持 9/15（60%），不得把本地 loopback 通过解释为 IMS 生产可用。
- 后续风险：macOS 27 SDK 对 `NSItemProvider.loadItem` 和若干 AVAudioEngine API 发出弃用警告；不是本次构建失败原因，后续应单独评估 API 替换并保留拖放、双向音频与 AirPods 回归。
