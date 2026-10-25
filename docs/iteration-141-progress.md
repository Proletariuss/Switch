# Iteration 141：公开网络诊断接口迁入 Rust

日期：2026-10-08。基线：Switch 2.92 / build 292。用户已批准全 Rust 后端作为持续项目目标。

## 实施

`GET /api/network` 现在由 Rust `switchd-rs` 直接处理。Rust 并行采集 macOS 网卡、默认路由与 DJI USB 设备观察，然后经 `module_radio` actor 顺序执行原有五条固定只读 AT 查询：`AT+QCFG="usbnet"`、`AT+QCFG="usbcfg"`、`AT+CGDCONT?`、`AT+CGACT?`、`AT+CGPADDR=1`。这些查询共用一次 Diagnostic reservation 与20秒总体期限；主机命令单次期限为5秒、输出上限为1 MiB。成功或查询失败后均尝试释放租约。

Rust 保留 `usbnet_mode`、`usbcfg`、PDP 上下文、活动上下文、PDP 地址、网卡、默认路由、USB 设备、`raw` 和逐命令 `errors` 字段。新增的部分完成标志、完成数、采集时间及耗时供诊断摘要使用；失败命令不丢弃其他成功结果。受限事件环只记录诊断阶段及粗粒度结果，不保存 AT 输出、网络地址或 USB 设备细节。

Go mux 注册、公开 handler、专用网络诊断文件和 PDP 解析器已移除。为保持其他 Go 操作的取消语义，将原来随诊断文件删除的局部锁等待函数整理为共享 `waitContextLock`，并用于 USB 生命周期、USB/PCSC 状态、短信存储与射频控制的原锁等待点；锁的实际持有和释放方式不变。

新增 Rust 投影测试覆盖响应解析、空字段和部分失败，actor 测试验证固定查询顺序、共享租约和释放。独立 Go ServeMux 冻结样本补入 `/api/network`，用于保留其历史 GET/HEAD 与错误方法语义。

## 验证

- Rust workspace：**1,318通过、0失败、14忽略**；daemon **833通过、0失败、3忽略**。
- Go 全仓测试、主包 race 测试和 `go vet ./...` 通过。
- Rust carrier replay 与 Go 对照 **2,722项**；USB状态、射频投影、codec/SDP parity通过。
- HTTP方法和HEAD逐字节对照：**359个原生路由案例、12个未改变的回落案例**通过。
- 迁移工具自测 **42项**、契约清单扫描、Rust格式和 `git diff --check` 通过。
- Rust契约共277条：Rust 199、Go 78，含31个派生HEAD；127个Swift调用点和225个审阅范围。
- `Switch.app` **2.93/build293 arm64** 已构建并通过 deep/strict 签名校验。`switchd-rs` SHA-256：`6f3dd6128d1f595adc3bc3a74acad503ec004f734a9a8e04cabcea5b21330030`。

Rust-only路由检查仍因默认 Go fallback 和147条存在Go所有权/依赖的路由失败；本轮不声称Go-free构建完成。构建与离线 fixture 没有调用真实 AT 命令，没有操作模块、读卡器或SIM，也未生成 DMG。需要在用户设备与实际网络下验证诊断快照。

## 进度

P2B 计入 **+0.025计划人周**：4.600 → **4.625/8.0（57.81%）**。整体由13.070增加至**13.095/47.5≈27.6%**（估算误差约±3个百分点）。当前迁移还没有完成 Go 进程退出、实机验收和最终 Go-free 干净源码交付。
