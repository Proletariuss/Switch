# Iteration 605 — Reader eSIM 下载 owner 闸门生命周期

日期：2026-10-21。唯一有效工作区为 `/Users/StatTrak/Desktop/Switch_MAC/`，源码在 `src/`，构建与验收证据在 `build/`，发行物在 `dist/`。本轮没有生成 DMG。

## 修复内容

审查 `reader_esim_download::start` 时确认：读卡器下载先取得 `DownloadPermit`，随后启动 detached Tokio worker；原代码只把下载确认接收端、取消标记和 operation id 用于 worker，permit 中的 `_operation_gate` 会在 HTTP 请求返回时释放。操作日志可能仍处于 running，阻止另一条 reader 下载，但 USB 模块写入路径通过共享闸门单独准入，所以在 reader 下载仍释放物理读卡器租约或刷新 Profile 列表时，模块写入可能通过另一条准入路径。

现已将 `_operation_gate` 从 `DownloadPermit` 显式移交至 detached worker。worker 完成物理任务、停止租约 heartbeat、释放 reader lease、记录最终结果并执行成功后的最后刷新后，才释放闸门。模块下载本身原来已将相同 permit 持有到 release 与 refresh 结束，因此无需改变其行为。

新增 `reader_download_gate_stays_held_until_owner_cleanup_finishes` 回归，验证 journal 可先成为终态，但共享操作闸门会继续拒绝 module mutation，直到 owner 明确结束清理。测试日志为 [`iteration-605-reader-esim-download-tests.log`](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-605-reader-esim-download-tests.log)，SHA-256 `f4fea48f961a50921bcf146cddbcf33493e685475d74a23fd0b41159cf4668ff`。本模块 51项测试通过、0失败、0忽略；`cargo fmt --all -- --check` 通过。源文件 SHA-256 `666f2d3674e0e796ba32a6cc40c472a8c36f12466335a34436199b59ddb02009`。机器可读结果见 [`iteration-605-reader-esim-owner-gate.json`](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-605-reader-esim-owner-gate.json)。

## 验收边界与剩余项

本轮是软件级并发生命周期修复，不是实体 eSIM 下载验收。没有执行 LPA 下载、Profile 切换、短信、电话或读卡器变更。daemon 关停期间如何等待 detached LPA 外层任务完整结束、并将 metadata/refresh 终态和 joined receipt 绑定，仍是未闭合项；P0–P6整体验收继续开放。验收台账没有新增需求、适用实例、功能 PASS 或冻结分母，故不计算总体百分比。

项目目标文档已以 `/Users/StatTrak/Desktop/Switch_MAC/` 作为当前工作区，路径分为 `src/`、`build/`、`dist/`；早期 `/Users/proletarius/Desktop/Switch/` 只作为历史记录。Codex 内部 goal objective 的旧路径字段仍不能由当前 goal 接口改写，该限制不影响本项目源码和构建脚本均按 `Switch_MAC` 工作。
