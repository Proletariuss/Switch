# Iteration 542：完整应用连续启动与 BLE socket 清理

日期：2026-10-18。

从 Iteration 541 的 Debug `Switch.app` 复制隔离测试包，临时改用 `com.proletarius.switch.smoke542` Bundle ID 并重新临时签名。两次启动使用同一个隔离 `SWITCH_DATA_DIR`；预先设置测试偏好以避免首次开机自启和更新检查。未安装或替换正式应用，也未使用正式数据目录。

第一次后台启动时观察到 SwiftUI 主进程及**该测试包内**的 `switchd-rs` 子进程，`switch.sock` 与 `estk-ble.sock` 均建立且权限为 `0600`。通过应用的 Apple Event 正常退出后，两个进程和两个 socket 都消失。第二次在同一数据目录启动得到相同结果；后端无应用令牌的 `/api/v2/runtime` 请求返回 401，BLE 桥收到错误令牌时返回 `ble_unauthorized`。再次正常退出后进程与 socket 均清理，随后删除隔离应用、运行目录及测试偏好。

脱敏证据在 `build/reports/iteration-542-two-launch-sanitized.json`。这证明 Iteration 541 的 BLE 清理逻辑在完整 SwiftUI 生命周期中生效，并且下次启动可重新绑定监听路径。测试采用后台启动，没有操作读卡器或 VoWiFi；它不证明 B0/B1 实机重连、当前安装包升级、通知权限、SIM World IMS/短信/通话或长时间稳定性。未生成 DMG，整体完成率不因单项收口测试换算为百分比。
