# Iteration 297：短信完整快照桥退役

日期：2026-10-09  
项目：Switch_MAC 全 Rust 后端迁移

## 目标

去除短信列表工作流中 Go 与 Rust 之间的整份易变 inbox 快照同步，避免Rust公开列表依赖Go重复缓存，并把必要删除同步缩减为稳定消息ID。

## 实施

- Go不再启动时或每次实体删除后发布整份SMS inbox快照；归档写入仍在耐久存储成功后发生。
- Rust移除旧的 `/api/internal/sms/inbox-snapshot` 接收端及对应body budget；archive ingest/delete内部处理器不再接收未使用的 `InboxState`。
- Rust短信公开列表、状态计数和本地删除选择使用耐久archive与Rust USB预览。
- Rust清空/删除完成后仍通过认证的Go兼容适配器发送所选稳定ID，仅修剪Go进程中的重复内存缓存。该本地缓存仍服务显式Go兼容路径和设备短信事务。
- USB/读卡器物理短信删除、Go短信接收生产者、IMS入站与RP-ACK、archive写入调用仍留在现有路径中；没有把本轮描述为短信端到端Rust化。

## 验证

- `go test -count=1 ./cmd/proletarius-macos`：通过。
- `CARGO_NET_OFFLINE=true sh scripts/test-migration-gates.sh`：通过；执行时daemon测试1189通过、3忽略，路由方法/HEAD及IMS兼容门禁通过。
- `python3 scripts/check-rust-migration-contracts.py`：通过；335个method/path契约，Rust273、Go62，127个Swift HTTP调用点，326个审阅范围，含49个GET派生HEAD别名。
- `git diff --check`：通过。

## 取舍与未完成

保留认证的Go稳定ID修剪适配器，因为Go兼容缓存仍有其他生产和兼容消费者；移除它需要先迁走短信接收/事务状态及兼容启动路径。故本轮只降低快照耦合，不计加权迁移进度。

当前整体进度仍为 **31.4%**（估算误差约±3个百分点）。Rust-only退出门禁仍被默认Go fallback和剩余Go路由/依赖阻塞；完整Go-free构建、短信/IMS实机验收继续未完成。9eSIM `6982` 按用户指示不再处理。本轮未访问实体读卡器、未发送短信/拨号、未生成DMG。
