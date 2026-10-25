# Iteration 616：麦克风处理算法移入 Rust

2026-10-21。**整体阶段仍为 P0 未闭合、P1–P4 实机验收中、P5 音频完善中、P6 最终验收未收口。** 接口归属覆盖仍为 294/294；验收分母尚未冻结，因此不报告整体百分比。

## 改动

读卡器 VoWiFi 上行音频原来由 Swift 完成 100 Hz 高通、噪声底自适应、噪声门、增益平滑与 0.90 峰值限幅。这些属于产品信号处理逻辑，不是 CoreAudio 系统适配。本轮将处理器迁入 `proletarius-audio-core`，以有状态 C ABI 在每个 20 ms PCM 帧上处理；Swift 保留 CoreAudio 采集、格式转换、调度和 macOS Voice Processing 能力检测。

通话停止时销毁 Rust 状态；音频路由变化时重置滤波与门限瞬态状态，但保留历史实现会保留的自适应噪声底值。Swift 在 Rust 句柄缺失或 FFI 返回无效时会丢弃该帧并记录诊断，不发送未处理帧。

## 验证

- 将旧 Swift 实现作为确定性参考，对静音、低噪声、弱语音、强语音、限幅、系统语音处理开关及状态重置共 18 个连续帧输出做 FNV-1a 字节级核对；Rust 向量全部一致。
- `proletarius-audio-core`：9 项测试通过，0 失败；包含旧 Swift 黄金向量、路由重置保留噪声底及 FFI 无效句柄/采样率/处理/销毁回归。
- `scripts/test-migration-gates.sh`：完整门禁通过，Python 100/100，Rust workspace 汇总 3,930 通过、0 失败、9 忽略（daemon 3,052 通过、8 忽略），BLE 管理器 41 组通过，冻结协议回放 297 次；Go 退出和 Rust owner 契约检查通过。
- `cargo fmt --all -- --check` 通过。
- `scripts/test-call-audio-store.sh`：31 个异步通话状态场景及 CoreAudio 退役账本检查通过；它使用模拟音频/API，不构成实机通话验收。
- 验收台账检查恢复为 `metadata_consistent=true`、错误 0；数量保持 API 294、Swift 调用 131、runtime 56、需求叶子 115、场景 21。整体分母仍未冻结，`functional_acceptance_awarded=false`。
- Xcode Release arm64 `.app` 构建成功；应用内音频处理器 4 个 FFI 符号均能在最终 Mach-O 中解析，`codesign --verify --deep --strict` 通过。未生成 DMG。

构建时 Xcode 报告若干 macOS 27 API 弃用警告及既有 Rust 未使用代码警告，没有阻断构建。工具链自带 `nm` 无法读取 Rust LLVM 22 对象中的新属性；改为检查成功链接后的应用 Mach-O，四个 FFI 符号均存在。

## 尚未验收

算法逐帧等价与软件构建不能证明设备上的话音清晰度、回声抑制、断续、蓝牙/USB 路由切换或长时间稳定。本轮未发起实卡呼叫，仍需在用户授权的 SIM World/ESTK 场景进行真实通话听音与路由恢复验收；P0–P6 其余门禁、验收台账冻结和三轮交叉审议也未完成。此工作包不关闭 P5 或整体项目。
