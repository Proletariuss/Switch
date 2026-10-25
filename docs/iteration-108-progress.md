# Iteration 108 — 4G 默认路由检查迁入 Rust

日期：2026-10-08。继续执行[全 Rust 后端改造目标](optimization/2026-10-04-all-rust-backend-plan.md)。本轮只读读取 macOS 网络状态；未更改网络配置、未操作读卡器或号卡、未生成 DMG。9eSIM `6982` 仍排除。

## 已完成

- Rust 接管 `POST /api/network/check-4g`：在有界子进程期限和输出上限内运行 `/sbin/route -n get default`，复用 Rust `ifconfig` 解析，按原有默认出口接口、`en0` 例外、ethernet 类型和 active 状态规则生成响应。
- 保留原有 `{ok, summary, detail}` 字段、中文结果、默认路由缺失说明和失败详情。该检查本身没有 Go 状态依赖；Go 恢复期间也由 Rust 本机认证分发处理，不会被误当成可能重复执行的写请求。
- Go 移除 `POST /api/network/check-4g` 注册、handler 和专用响应类型。更广泛的 `GET /api/network` AT诊断以及网络服务顺序 `PUT` 仍留在 Go。
- 复用的路由检查沿用旧判断语义：它根据默认出口是否为活动的非 `en0` 以太网接口推断 4G，并未检查 network service 是否被标记为模块；本轮未改变这一既有行为。

## 验证

- Rust `proletariusd` 全量：**709 passed、0 failed、3 ignored**。之后追加的 POST/405 兼容断言通过定向重跑；`cargo +1.98 fmt --all -- --check` 通过。
- Rust 网络检查定向测试 **4 项通过**；网络流量解析/基线测试 **6 项通过**。Go `go test ./cmd/proletarius-macos -count=1` 通过，保留一条已有重复 `libusb` 链接警告。
- 契约扫描通过：**257** 条 method/path（Rust **169**、Go **88**，含 **26** 条 HEAD 派生别名）、**201** 个指纹审阅范围、**127** 个 Swift HTTP 调用点；扫描器 **41 项自测通过**。Rust-only 门禁仍因其它 Go fallback/依赖而受阻。
- `scripts/build.sh` Release 构建及 `codesign --verify --deep --strict` 均通过，产物为 `dist/Switch.app`（2.86/build 286，arm64）。未执行实时路由探测或网络配置变更。

## 进度与后续

将本地路由采样、响应投影及 Go handler 删除按 **0.025 计划人周**计入 P2A，P2A 约 **62.91%**；整体为 **11.970 / 47.5 ≈ 25.2%**（±3 个百分点）。新增 Rust 路由数只反映入口 owner，不代表整个网络管理或 Go backend 已迁移。网络顺序写入、完整网络诊断、Go 进程和硬件业务接管仍待完成。
