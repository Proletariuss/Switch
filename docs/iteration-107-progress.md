# Iteration 107 — 网络流量采样迁入 Rust

日期：2026-10-08。继续执行[全 Rust 后端改造目标](optimization/2026-10-04-all-rust-backend-plan.md)。实现与构建均在 Switch 工作区内；未更改网络服务顺序、未操作读卡器或号卡、未生成 DMG。9eSIM `6982` 仍排除。

## 已完成

- Rust 接管 `GET /api/network/traffic`：限时、限输出地调用 macOS `networksetup`、`ifconfig` 与 `netstat`，解析服务/网卡和 link byte counters，保留接口响应结构及既有中文状态。采样只选明确标记为当前模块的网络服务，不会把扩展坞网卡当作模块流量；网络查询错误保留 `networksetup:` 前缀。
- Rust 维护网卡会话流量基线，遇到计数器归零或后端代次变化时安全重置。服务枚举复用上一轮 Rust parser。
- Go 删除流量 GET 路由、handler、计数响应类型和进程内基线；Go `PUT /api/network/services-order` 仍负责网络顺序变更。
- 为兼容旧 `module` 标记，Rust 仍从 Go `GET /api/health` 读取 USB 产品名。GET 派生的 HEAD 继承该依赖，因此这不是 Go-free 路由或完整网络管理迁移。

## 验证

- Rust `proletariusd` 全量：**704 passed、0 failed、3 ignored**；`cargo +1.98 fmt --all -- --check` 通过。
- Go `go test ./cmd/proletarius-macos -count=1` 通过；保留一条已有重复 `libusb` 链接警告。
- 契约扫描一致性通过：**257** 条 method/path（Rust **168**、Go **89**，含 **26** 条 HEAD 派生别名）、**200** 个指纹审阅范围、**127** 个 Swift HTTP 调用点。扫描器自测 **41 项通过**。Rust-only 门禁仍因 Go fallback 和明确的 Go 依赖而阻塞。
- `scripts/build.sh` Release 构建成功：`/Users/proletarius/Desktop/Switch/dist/Switch.app`，**2.86 / build 286 / arm64**；`codesign --verify --deep --strict` 通过。构建保留已有 Rust dead-code 与重复 `libusb` 链接警告。
- `git diff --check` 通过。未做实时网络流量或实体设备验证；没有执行网络配置变更。

## 进度与后续

将只读采样 handler、解析、会话状态及 Go 原实现删除按 **0.040 计划人周**计入 P2A，P2A 从约 **61.73% 调整至 62.45%**；总计 **11.945 / 47.5 ≈ 25.1%**。百分比按一位小数显示，故本轮实际增量尚未改变显示值。路由数量不替代全功能进度。Go health 身份依赖、服务顺序写入、设备生产链以及全 Go-free 构建和设备验收仍未完成。
