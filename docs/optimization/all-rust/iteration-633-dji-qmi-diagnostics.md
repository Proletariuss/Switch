# Iteration 633 — DJI 一代 QMI 初始化诊断

用户确认设备为“正常的 DJI 一代模块”。本轮沿用 3.46 / build 346 的 DJI QMI 启动路径，修复初始化失败时界面缺少原因的问题。没有改写模块 USB 模式、发送 AT/QMI 控制命令或生成 DMG。

## 设备身份与实现边界

- DJI 一代模块的常见 USB 身份为 `2ca3:4006`；Rust QMI 自动选择表已包含此 VID:PID。接口筛选只接受厂商类 `ff/ff/ff`、一组 bulk IN/OUT 及 interrupt endpoint，已通过候选选择单测。
- 外部研究记录描述 DJI 私有 USB composition 中 interface 4 为 QMI control 候选，同时报告在 DJI 私有模式下标准 QMI 探测可能超时。这是第三方逆向记录，并非用户设备的实测结论；不能仅凭 VID:PID 断言 CTL_SYNC 会成功。
- 本轮修复的是诊断可见性。Rust 在 QMI 初始化失败后保留经过 240 字符上限和控制字符清理的原因，并通过 `/api/status` 提供给首页与菜单栏；成功建立会话后清除旧失败原因。SIM、注册、信号和模块在线状态仍由独立的实际控制响应决定。
- 当前 Rust QMI 路径只启动 IMS/NAS/UIM typed control owner；它不建立蜂窝数据会话，也不能替代模块串口 AT owner。若日志指向 DJI 私有模式的 CTL_SYNC 超时，后续应基于实机 USB 接口描述符和 AT 端口观察确定传输适配，不能把切换 USB composition 当成无风险修复。

## 验证

- QMI owner 生命周期及错误缓存：31 项通过。
- QMI USB 选择和候选接口：6 项通过。
- 状态投影：3 项通过，失败原因不会将 SIM 或运营商状态提升为就绪。
- API contract：9 项通过，1 项忽略（需 Swift handoff 专项脚本）。
- 菜单栏：16 个面板渲染、8 种显示选项通过；增加了 CTL_SYNC 失败文案场景。
- 设备探测：当前主机的 USB 清单没有 DJI/Quectel 设备，Switch/switchd 进程也未运行；因此未执行真实 CTL_SYNC、SIM 读取或网络验收。
- Release 构建已完成，生成 `/Users/StatTrak/Desktop/Switch_MAC/dist/Switch.app`；版本 3.46/build 346，arm64，签名 `codesign --verify --deep --strict` 通过。包内 backend 只含 `switchd-rs` 与 `libusb-1.0.0.dylib`，switchd-rs 嵌入当前源码摘要 `d5fcd876bbc69cb909868fdd3a63ce6eedd30256f27ad629bca7b6517243780b`。产物与各项 SHA-256 记录见 [`iteration-633-app-verification.json`](../../../../build/reports/iteration-633-app-verification.json)。本轮仅产出 `.app` 候选，没有生成 DMG。

参考：

- [DJWebCfg：一代 PID 0x4006、二代 PID 0x4009](https://github.com/simonire/DJWebCfg/blob/main/README.md)
- [DJI Cellular as Modem research notes：USB interface 与私有模式 QMI 探测记录](https://github.com/CdricZhang/dji-cellular-as-modem/blob/main/RESEARCH_NOTES.md)
- [DJI 4G Dongle Toolkit：QDC507 与 2CA3:4006](https://github.com/maomomo-eth/dji-4g-dongle)

本轮没有把“识别到设备型号”或模拟测试当作硬件验收通过。P0–P6 仍未全部闭合，验收总分母尚未冻结，不报告项目整体完成百分比；最终三轮交叉审议、实机验收及 DMG 均未完成。
