# Iteration 320：BLE 租约申请结果不确定时的释放

2026-10-10。完成并交付 Switch 2.96 DMG 后继续全 Rust 目标，本轮没有再次生成安装包。

Rust 在申请 Go BLE compatibility lease 的第一个 RPC await 之前安装到期清理任务。即使 Go 已取得租约但连接中断、响应损坏、返回不同 token，或等待方取消，也按原 Rust token 保留清理；只有明确的客户端拒绝才取消清理。没有重放 connect/disconnect。

`acquire_supervised_lease` 是生产处理器与回归共用的路径。真实本地 Unix socket fixture 覆盖 EOF、5xx、损坏成功响应、不同 token、申请过程取消、明确 4xx 拒绝不发 release；本阶段定向 **10 项通过**。后续 Iteration 321 增加物理通道互斥回归，并通过完整迁移门禁。

Go compatibility token/实时业务门禁仍在，整体加权进度 **31.4%**。未访问真实设备或运营商。
