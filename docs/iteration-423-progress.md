# Iteration 423：外呼 INVITE 初始 CSeq 分配迁入 Rust

日期：2026-10-12。Rust outgoing-call policy 现在分配进程内单调递增的初始 INVITE CSeq，并在创建 tracker 时绑定该序号。Go 不再递增本地计数来生成生产 INVITE CSeq；只校验 Rust 的返回值、镜像已分配值并负责 SIP wire 构造/写入。策略返回异常或 INVITE 构造/订阅失败时，runtime 失败关闭并尽力移除 Rust tracker。应急策略通道失效后的 2xx 清理和非 tracker 兼容路径仍保留 Go CSeq 处理。

新增 Rust allocator 唯一性/范围、Go bridge 禁止自带初始 CSeq 等回归。完整离线门禁通过：[iteration423-migration-gates.log](../build/optimization/iteration423-migration-gates.log)，SHA-256 `1b32412c7e564966e919f4543678fca1e75ec51bee026820dbd41a5b925c548f`；Rust 全 workspace、Go 全包 test/vet、Swift BLE 与协议 parity 通过。`go test -race ./internal/vowifi/imscore ./cmd/proletarius-macos` 通过，`git diff --check` 通过。API census 保持346 method/path（Rust286 / Go60）、127 Swift 调用点、370 review scopes。

P5 +0.025 计划人周等值，累计 **0.350 / 8.0**；全项目 **17.770 / 47.5 = 37.41%**（估值误差约±3个百分点）。结构退出审计仍有7类 Go 构建/运行/源码依赖及264个生产 Go 源文件。本轮没有真实读卡器、SIM、短信或运营商呼叫验收；9eSIM `6982` 仍排除。未生成 DMG；最新发行包仍是 Iteration 422。
