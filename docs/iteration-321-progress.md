# Iteration 321：剩余 Go BLE 卡片传输汇聚至 Rust

2026-10-10，用户要求“输出 DMG 后再继续推进”。2.96 阶段包已先交付，以下迁移是其后的源码改动，**没有包含在已冻结的 2.96 DMG 中**，本轮不再打包。

## 生产变化

- Go 的 USIM transmitter、eSIM logical channel 和切卡 reset 改调 Rust 私有 `POST /api/internal/reader-ble-transport`。必须指定预期 `BLE:<UUID>`，使用 `SWITCH_INTERNAL_SOCKET` 和内部凭证。缺少 Rust 配置时失败，不回退 Swift 直连，不重放未知结果的 APDU/reset。已删除 `estkble.Client.Exchange/Reset`；只读状态观察仍由 Go 查询 Swift。
- Rust 原生 LPA/AKA 和兼容入口共用按 Swift socket 隔离的物理 I/O permit。兼容请求在派发 blocking worker 前取得许可，覆盖状态预检和命令完成；HTTP 等待方取消不会释放仍在执行的物理命令。原生 transport 在其现有事务作用域内持有许可。公开 BLE 控制也检查许可，避免请求取消后立即切换设备。
- 每次 APDU/reset 在 Swift 桥再次绑定预期 reader。Rust 解析实际 `errorCode` 字段，保留已核对的 Swift 错误码白名单。
- Unix socket 连接使用已在锁文件中的 `socket2 0.6.5`，加入 3 秒连接上限；APDU/reset 沿用 135 秒 I/O 预算。Go channel/RPC 预算为 145 秒，reset 保留调用方 context。
- 退出时只允许已有、绑定 reader/generation 的 N待 reservation 借用清理许可，以恢复原 Profile。许可计入活动 worker，真实工作结束前不能释放或替换 reservation；不为普通新会话开放退出期间读卡。

## 交叉审议与验证

独立审查发现并修复三个问题：状态预检阶段取消导致命令被后来事务超车、N待退出恢复被新网关拒绝、socket backlog 饱和时连接无期限等待。补充本地 socket 故障回归并复核通过，也补齐实际 Swift 错误码白名单。

- Rust BLE 定向测试 **31 项通过**：精确传递 APDU/reset、reader/认证、错误语义、取消前后许可、退出清理、连接 backlog、未配置拒绝。
- Go `./cmd/proletarius-macos` 与 `./internal/estkble` 全量通过；新增 6 组 transport contract 测试，其中失败表含 8 个子例。
- 完整 `scripts/test-migration-gates.sh` 通过：Rust workspace 聚合 **1,758 passed / 6 ignored**，其中 daemon **1,229 passed / 3 ignored**；Go vet/全包测试和 carrier、USB、radio、HTTP method/HEAD、codec、SDP、IMS MESSAGE 差分通过。使用本机 Go 1.26.7 的 Switch/build 临时 shim，退出后清理，未修改全局 Go 配置。
- API 清单 **337 method/path（Rust 276 / Go 61）**、127 Swift HTTP 调用点、332 审阅范围；扫描器自身 **43 项通过**；`git diff --check` 通过。

日志：`Switch/build/optimization/2026-10-10-iteration-321-*`；Go adapter 日志为 `2026-10-10-reader-ble-native-go-full-tests.log`。

## 边界与下步

本轮接管 BLE 卡片命令的网络传输和物理 I/O 排他生命周期。Go 仍持有多命令事务、reader generation、会话/N待 admission、部分状态观察及兼容 lease，没有把窄 RPC 包装计为整个 reader actor 或 Go 退出完成。完整设备、运营商及长期验收尚未进行，整体加权进度仍 **31.4%**。

下一步将普通 reader 操作到 native session 的原子所有权转移，以及 N待预约生命周期统一到 Rust，再移除 Go 全局业务标志和 compatibility lease。须覆盖全部生产消费者，不能用先查询 busy 再执行替代原子准入。
