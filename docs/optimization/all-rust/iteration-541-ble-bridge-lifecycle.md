# Iteration 541：BLE 桥 socket 退出与重启边界

日期：2026-10-18。

Iteration 539 的完整 SwiftUI 烟测发现正常退出后 `estk-ble.sock` 留在磁盘。`ReaderBLEBridge` 先前没有显式停止入口；其绑定失败分支还会无条件 `unlink` 目标路径，在两个实例竞争绑定时可能删除另一实例刚创建的 socket。

现在应用终止时先停止 Rust 后端，再关闭 BLE 桥监听器；桥只在路径的设备号与 inode 仍匹配自身绑定记录时清理 socket。绑定失败不再删除路径，旧路径检查改用 `lstat`，拒绝覆盖普通文件和符号链接；接受循环按监听代次拒绝旧代连接。未修改 BLE/GATT、卡片协议或业务操作。

生产 `ReaderBLEBridge` 的无硬件测试通过：正常关闭移除自身路径、同进程重启后认证请求可用、路径被另一个监听器取代后关闭不会删除对方 socket、普通文件不被覆盖。`scripts/test-reader-ble-operation-proof.sh` 全部通过；完整 macOS Debug `xcodebuild` 构建通过，并重新编译嵌入 Rust 后端。构建日志：`build/reports/iteration-541-xcodebuild.log`；产物仅在 `build/iteration-541-derived/Build/Products/Debug/Switch.app`，未安装、未生成 DMG。

这项修复关闭了正常退出后的 BLE socket 清理缺口，但测试中的蓝牙管理器是替身；真实 9eSIM B0/B1 设备退出、重新启动和断线重连仍需实机验收。已安装旧版后端进程的健康超时、SIM World 的零 ESP 回程及运营商业务验收也不由本次修改解决。项目整体完成率不因这一收口小项重估；294/294 Rust HTTP 接口归属不是全功能 100%。
