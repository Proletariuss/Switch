# Iteration 166：N待操作账本与状态 API 迁入 Rust

日期：2026-10-08。所有变更与测试产物留在 Switch；本轮没有连接读卡器/SIM、访问运营商或真实 provider，也没有构建 App 或生成 DMG。

Rust 现在持久拥有 N 待 start/stop/retry 请求的请求指纹、幂等键、操作状态和公开状态投影。它在查运行时 Profile 前先检查重复请求，先耐久记录、再通过有界且认证的私有接口让 Go 执行既有会话操作。执行响应丢失时状态进入 `unknown`；daemon 重启会把 pending/running 隔离为 unknown，不自动重放。Go 保留物理卡/Profile 的会话生命周期，仅作为已接纳操作的执行器；本轮没有迁移 N 待 broker 或多 Profile VoWiFi/IMS。

为保持既有 HTTP 行为，恢复了冻结 Go mux 测试中的 start/stop/retry/operation-status 路由样本；迁移后仍验证方法、405/Allow 与 HEAD 兼容。修复 Go 测试 fixture 的缺失 `time` 导入，清除了遗留 Git 冲突状态，并把Rust转交的四条N待公开路由，以及新增的operation-claim/operation-transition两个私有回调均登记到契约清单。

验证通过：Rust daemon **958项通过、3项忽略**；Go 全仓 `vet` 与测试通过；载波回放 **2,722项**，USB 状态、模块 radio、HTTP/HEAD 和语音编码差分通过；Rust/Go 进程集成 **22项**通过，其中覆盖持久 N 待操作接纳、单次 Go 执行、私有回调和重复请求幂等；8项操作账本定向 Rust 测试通过。契约 `--diff`、Rustfmt、工作区/暂存区 `git diff --check` 通过，清单 **295条**（Rust **231** / Go **64**，含 **38** 个 HEAD alias）、127个 Swift HTTP调用点、277个审阅范围。Rust-only退出门禁仍因默认 Go fallback 与 **149条** Go owner/dependency 路由受阻。所有测试为离线合成测试，不证明实卡并发或运营商服务可用。

保守计入 P4 **+0.025计划人周等值**，由0.475增至**0.500/4.5（11.11%）**；总体由13.745增至**13.770/47.5≈29.0%**（估算误差约±3个百分点）。P2A维持3.885/5.5（70.64%），P2B为4.950/8.0（61.88%），P3为1.565/8.0（19.56%），eSIM/LPA为1.125/5.5（20.45%）。实际 N 待 session/broker、设备与SIM验收、全功能 Go-free build/runtime及长稳测试仍未完成。
