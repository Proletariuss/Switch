# Iteration 630 — DJI USB/QMI 模块未响应状态修正

用户确认 3.46 中无响应的 USB/QMI 模块是 DJI。本轮限定为软件验收，没有读取或操作实体模块、SIM、短信或通话。

代码检查发现两个会造成误判的问题：模块主页把“没有 `hardware_status` 错误”当作模块已连接；活动页和菜单栏又把 USB 枚举本身当作控制通道已工作。只读 IOKit 枚举只能证明 DJI USB 设备存在，不能证明 AT/QMI 请求成功。枚举结果里的厂商自定义接口类别也不足以判定 QMI 已就绪。

本轮将“设备已枚举”和“控制通道有响应”拆开：模块页、活动页和菜单栏只有在收到 IMEI、SIM 状态、信号、注册状态、运营商或网络制式等模块响应后才报告在线；DJI USB 可见但没有有效状态时显示控制通道未就绪，并把 VID:PID、接口模式和发现错误作为诊断信息。只读接口模式统一显示为 `vendor-specific USB interface`，不再从接口类别推断 QMI 已连接。通用健康枚举仍只匹配 DJI；没有扩展 Quectel 行为。

验证结果：Rust `network_diagnostic` 4/4、`module_discovery` 8/8；菜单栏 SwiftUI 测试通过，渲染 12 种原生面板状态，并覆盖“USB 已枚举但没有控制响应”的情况。当前证据不能证明 DJI 的真实 QMI 会话已经恢复：应用启动器未传 `-backend=qmi`，QMI owner 仍是可选控制接口，且模块页面没有把 QMI 运营商/IMS 查询接入完整模块业务状态。因此本轮修复的是错误在线判断和可诊断性，不把模拟/只读枚举结果记作实机连通验收。下一步仍需接入已确认的 DJI QMI 业务链，并在硬件可用后验证 QMI/AT 实际响应、模块状态和功能。

本轮没有 DMG，也没有授予任何功能验收 PASS。整体 P0–P6 仍未全部闭合，分母未冻结，不报告整体百分比；294/294 仅表示 Rust 接管的接口归属。三轮交叉审议仍待全部代码与功能验收完成后开展。

证据：`../build/reports/iteration-630-network-diagnostic-tests.log`、`../build/reports/iteration-630-module-discovery-tests.log`、`../build/reports/iteration-630-menu-bar-presentation.log`。

候选应用也已完成 Release 构建：`/Users/StatTrak/Desktop/Switch_MAC/dist/Switch.app`（3.46/build 346，arm64）。`codesign --verify --deep --strict` 通过；包内后端为 `switchd-rs` Mach-O arm64 与 `libusb-1.0.0.dylib`，没有旧 `switchd` Go 后端。构建日志：`../build/reports/iteration-630-app-build.log`。本候选没有启动，因此不含硬件连通结果。

构建后缓存清单发现另一个工程仍有活动中的 Xcode 构建；清理器会在任何构建进程活动时拒绝删除。本轮按此保护保留编译缓存，未绕过清理器。
