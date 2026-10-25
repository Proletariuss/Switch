# Iteration 631 — DJI 一代模块 QMI 启动接线

用户确认设备为 DJI 一代 USB/QMI 模块。本轮依据此前“仅做软件验收”的约束，没有操作读卡器、实体卡、短信或通话，也没有生成 DMG。

## 修复

Rust 的 USB QMI 选择器已包含 DJI VID:PID `2ca3:4006`，并按厂商接口签名、bulk 方向和 interrupt endpoint 筛选，再通过 QMI CTL_SYNC 验证接口；但 Swift 启动器只传 `-listen`，Rust 因默认配置 `at` 而从不启动 QMI owner。本轮在唯一 Rust 子进程启动参数中增加 `-backend=qmi`。这会让现有可选 QMI owner 在后台尝试建立服务客户端；不支持的设备或 QMI 启动失败仍不阻塞公共 API/AT 服务就绪。

Swift readiness 测试的合成后端现在断言每次启动都带 `-backend=qmi`。Rust 公共 HTTP 路由没有改变，清单仍为 294 个 Rust owner、0 个 Go owner；QMI 探测结果尚未接入模块页的状态投影，因此这里确认的是 QMI transport 初始化路径已接线，不代表模块业务链或实机恢复。

## 验证

- `scripts/test-backend-readiness.sh`：通过，包含子进程启动参数断言、启动/停止和健康检查路径。
- Rust QMI 专项：38/38 通过。
- Rust socket 生命周期专项：17/17 通过。
- `scripts/preflight-source.sh` 与路由/调用清单一致性：通过；294 Rust / 0 Go，129 Swift HTTP 调用点、467 review scopes。
- 全量迁移门禁中的 workspace 测试为 3,056 通过、1 失败、8 忽略；唯一失败是并行运行时 `socket_lifetime::tests::same_path_is_exclusive_until_drop_and_empty_companion_is_never_unlinked` 在重新获取已释放锁时遇到 `Busy`。单测单独重跑通过，`socket_lifetime` 组以单线程运行 17/17 通过。故完整 workspace 门禁仍记录为有一次并行瞬态失败，不能记全绿。
- Release `.app` 构建成功：`Switch.app`，3.46/build 346，arm64，ad-hoc deep/strict codesign 通过；包内 Rust daemon 为 arm64 Mach-O，并链接随包的 `libusb-1.0.0.dylib`。本候选没有启动。

## 验收边界与后续

候选位于 `/Users/StatTrak/Desktop/Switch_MAC/dist/Switch.app`。需在设备实测中检查后端日志是否成功完成 `2ca3:4006` 的 QMI CTL_SYNC 和 IMS/NAS/UIM client 初始化，并确认模块页能读取真实模块状态。若 transport 初始化成功但页面仍无响应，下一项应是把 Rust QMI probe/状态安全地接入现有 `/api/status` 投影与 UI；不能把本轮启动接线当作完整模块控制或功能验收。

本轮未改变版本号，不产生 DMG，也不授予硬件功能 PASS。整体 P0–P6 未全部闭合，需求分母未冻结，故不报告整体完成百分比；294/294 仅代表后端路由归属。

证据：`../../../../build/reports/iteration-631-backend-readiness.log`、`../../../../build/reports/iteration-631-qmi-tests.log`、`../../../../build/reports/iteration-631-socket-lifetime-tests.log`、`../../../../build/reports/iteration-631-dji-qmi-app-build.log`。
