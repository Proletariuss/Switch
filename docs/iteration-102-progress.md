# Iteration 102 — eSIM 本地备注持久化迁移到 Rust

日期：2026-10-08。继续执行 [全 Rust 后端改造目标](optimization/2026-10-04-all-rust-backend-plan.md)，本轮只改动 Switch 工作区。未操作实体读卡器或号卡，未发送短信，未生成 DMG。9eSIM `6982` 继续排除。

## 已完成

- Rust 接管 `GET/PUT /api/esim/notes`，复用旧 Go `profile-notes.json` 的对象格式和字段，现有备注无需转换。
- Rust Store 统一读取、更新、删除及号码回填。写入使用本地互斥和文件锁、私有目录/文件权限、受限体积、原子替换与目录同步；拒绝符号链接、非对象及损坏数据，读取不存在的文件不会创建文件或锁。
- Go 移除公开备注 handler、内存缓存和文件 writer。仍由 Go 驱动的 SIM 号码消费者通过应用 token 认证的 Rust UDS 私有 `phone` / `remember-phone` RPC 读写，防止出现两个持久化 owner。
- eSIM 备注迁移只覆盖本地备注和号码回填。LPA 下载/安装、Profile 管理、读卡器业务与 IMS 流程仍需分别迁移；9eSIM `6982` 不在工作范围。

## 验证

- Rust daemon 全量：**678 passed、3 ignored、0 failed**；本轮新增 8 项 `profile_notes` 测试。
- Go `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local go test ./cmd/proletarius-macos -count=1`：通过。
- API 契约检查及 41 项 Python 检查器自测：通过。清单为 **252** 个 method/path（Rust **155**、Go **97**，含 **21** 个 HEAD 派生别名）、**194** 个审阅范围和 **127** 个 Swift HTTP 调用点。
- Rust-only 退出检查按预期失败：默认 Go fallback 仍在，另有 **158** 条路由仍带 Go 所有权/依赖；这不是本轮失败项，而是完整迁移尚未完成的准确证据。
- `cargo +1.98.0 fmt --manifest-path rust/Cargo.toml --all --check`、`git diff --check`：通过。
- `scripts/build.sh` Release 构建、产物签名验证：通过，生成 `/Users/proletarius/Desktop/Switch/dist/Switch.app`。日志：`build/optimization/iteration-102-app-build.log`。日志中保留若干 dead-code、macOS 27 弃用及重复 `libusb` 链接警告，本轮构建无错误。

其他日志位于 `build/optimization/iteration-102-{rust-tests,go-tests,contract-diff,contract-check,contract-selftest,rust-only}.log`；Go 测试日志为 `build/optimization/iteration-102-go-tests.log`。

## 进度与后续

全项目按现有 47.5 人周粗估维持 **约 24.9%（±3 个百分点）**。本次只切换 eSIM 备注持久化这一小块，不将整个 eSIM/LPA 工作包计为完成，也不以 API 数量换算总进度。下一步继续选取可独立迁移且有明确旧消费者边界的 Go 业务；最终需通过无 Go fallback 构建、完整功能与硬件验收后才能报告 100%。
