# Iteration 600：验收台账一致性与 3.46 发行

2026-10-20。本轮修正了 P0 验收台账校验中的两类结构识别：跨审阅组复用且内容完全一致的 source pin 不再误报冲突；运行时 source-map 的历史片段关系从实际记录它的 `current_anchor` 读取，并检查关系类别与片段对应。为此增加了正向和负向回归。`test_acceptance_ledger.py` 的42项测试通过，台账一致性检查通过；294条 API、131个 Swift 调用点、56条 runtime 记录、115个需求叶子和21个场景类别均被识别。总体验收分母仍未冻结，不计算总体百分比，也没有因此授予功能验收通过。

按用户要求，从当前 Switch_MAC 源码构建并输出 [Switch 3.46/build 346 DMG](../../../../dist/Switch-3.46-macOS27.dmg)。Xcode Release 构建、DMG校验、只读挂载、版本/架构、app deep/strict 签名、包内 Rust daemon 和发行脚本隔离测试通过。签名为本机 ad-hoc，未公证。Go 后端退出结构检查通过。

**未通过项仍保留。** `check-rust-migration-contracts.py --require-rust-only` 要求人工复核 `rust/crates/proletariusd/src/sms_forwarding.rs` 的整文件审阅指纹，故本轮严格 Rust 路由门禁退出码为1；不能把此状态改写为全绿。没有实机/运营商/SMS/通话验收。P0未闭合，P1–P4验收中，P5完善中，P6未完成；总体百分比不计算。
