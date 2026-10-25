# Iteration 061 — Reusable per-reader operation gate

Date: 2026-10-05  
Goal: Switch 全 Rust 后端改造；参考 Celmux，未生成 DMG。

## 本轮实现

- 在 `proletarius-reader` 增加 `with_reader_operation`，将长时间 APDU/PCSC 流程接入既有的 process-wide、per-reader try-lock。
- 同一读卡器已有操作时立即返回 `SharingViolation`，不在 HTTP 请求或后台队列中延迟执行卡片写入；不同读卡器可并行。
- 锁由闭包作用域持有，返回/展开时自动释放。API 文档明确要求调用方另行持有跨进程 reader lease，且不得把 APDU 工作拆到脱离锁的任务中。
- Celmux 的 Rust 应用层 operation 设计已补入复用评估：可借鉴 bounded progress/cursor、短期终态保留、worker 和确认 callback 生命周期；当前 UI 仍走 Switch 轮询契约。Celmux app operation 源码标为 PolyForm Noncommercial，只参考设计，不复制；MIT LPA/eUICC 协议实现继续复用。

## 验证与边界

- `cargo +1.98 test --offline -p proletarius-reader`：26 项通过，0 失败；新增测试覆盖同 reader 拒绝、不同 reader 并行与正常返回后释放。
- `cargo +1.98 fmt --all -- --check`、`git diff --check` 通过。
- 未调用生产 reader endpoint；未打开 PC/SC、读写实体卡或访问 SM-DP+。该锁尚未被下载 route 调用，Go 跨进程 lease、operation/恢复账本、下载 UI 确认仍需接线。
- 整体进度仍为 **17.5% 已完成 / 82.5% 待完成**（8.305/47.5 计划人周，误差约 ±3 个百分点）。此轮只建立可复用互斥接缝，没有生产 owner 的行为迁移证据，不增加百分比。

详情见 [Celmux 复用评估](optimization/2026-10-04-celmux-reuse-review.md) 与 [全 Rust 实施记录](optimization/2026-10-04-all-rust-execution.md)。
