# Iteration 432：模块语音运行时下载入口与进度监督迁入 Rust

日期：2026-10-13。全 Rust 后端目标继续进行中；本轮只迁移 call-mode 运行时下载的公开入口与任务监督，不宣称模块语音端到端 Go-free。

## 实际变更与职责边界

- Rust gateway 现在直接处理 `POST /api/call-mode/download`：保留 App 本机凭证、公有方法/请求体预算门禁，严格验证用户确认和 `github`/`relay` 来源，生成随机操作 ID，再调用带内部凭证的 Go 准入适配器。
- Rust 已有 hash-pinned 下载器接管启动、固定来源/清单、下载工作线程、文件完整性检查、进度采样及终态回调重试。Go 原 public handler、下载轮询器和旧私有 Rust GET/POST 下载入口已移除。
- Go 继续刷新模块语音状态、保留既有射频/USB 操作锁、接受有界单调进度，并在 Rust 报告完整校验后执行现有硬件部署。26 分钟 Go watchdog、模块 readiness、部署与硬件生命周期仍属 Go 运行依赖；下载请求未在真实上游发起。
- 交叉审阅发现并清除了 `ownership.rs` 对退役私有下载 GET/POST 的残留 Rust-owner 分类，并增加测试确保旧路径回到 Go compatibility 分类。契约记录现在明确列出 Rust public route 对 Go admission/progress/deployment 的依赖。

## 验证与未完成边界

- `scripts/test-migration-gates.sh`：Rust workspace、Go 全仓测试/vet、Swift BLE 回归与 carrier/USB/radio/HTTP/codec/IMS 跨语言 parity 全部通过；最终日志 SHA-256 `6ef8607bec2495d358a28abb258200a0de9e4a30da3f8328481ef5279685a92a`，文件位于 `build/reports/migration-gates-iteration432-final.log`。契约 diff 日志 SHA-256 `7351d5178a6109bf2b43422bc442a63d1808ec22140b42075da836c13785489b`；44 项工具测试日志 SHA-256 `0365eb107e52772c2859cd7c32fe845e95202ef9227dc1bd93f4c523ed58f186`。
- 契约工具 44 项测试通过；更新后的 `api-contract-inventory.json` 与实际源码差异检查通过。清单为 **349 method/path（Rust 289 / Go 60）**、127 个 Swift 调用点、373 个 reviewed scope。
- Rust owner 回归测试 `ownership::tests::native_reads_and_selected_reader_esim_routes_are_rust_owned` 通过；Rustfmt 检查通过。
- 新增 Rust 公共下载入口合成 UDS/HTTP 集成测试：固定来源与随机操作 ID 进入 Go 硬件准入，随后 Rust 本地启动失败通过同一操作 ID 回报 Go。测试使用不存在的数据目录，在真实网络请求前失败，不触碰模块或真实下载。
- Go backend exit audit 仍有 **6 类**构建/运行/源码阻塞，生产 Go 源文件数 **265**。无真实下载、模块部署、读卡器/运营商验收；不涉及 9eSIM `6982`。本轮不生成 DMG。

## 进度记账

本轮将公开下载确认/编排及异步进度监督从 Go 移至 Rust，计入 P2B **+0.025 计划人周等值**。累计 **17.945 / 47.5 = 37.78%**，估算误差约 ±3 个百分点。此分值不包括已在 Iteration 129 计入的下载、缓存和哈希能力，也不代表 Go 已退出 call-mode。

阶段 DMG `Switch-3.11-macOS27-iteration432.dmg` 在本轮契约复核修正之前生成；其下载路径实现已在包内，但退役私有路径的 owner 分类清理和新增断言只存在于后续源码工作区。本轮未重打包。发行记录：[release-3.11-iteration432](release-3.11-iteration432.md)。
