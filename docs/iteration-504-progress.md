# Iteration 504：读卡器 eSIM 原生接管与旧 Go LPA 退役

2026-10-16。当前源码目录为 `/Users/StatTrak/Desktop/Switch_MAC/src`。本轮完整软件门禁已通过；没有生成 DMG、更新版本号或操作真实号卡。最新安装包仍为 3.42 / build 342，包含 Iteration 503。

## 实现

读卡器 eSIM 概览、健康检查、操作状态与下载确认直接使用 Rust owner，不再等待 Go ready，也不再在未选卡或原生读取失败时回退 Go。PCSC 与 BLE 复用现有 APDU、生命周期及实际清理路径；下载、重命名和删除后的刷新通过原 reader 与 generation 的本地生命周期命令执行。非原生管理的模块仍通过 `/api/esim/operation` 观察忙态，避免串口/QMI 模块操作冲突；该条件依赖明确保留。

概览只有在 EID/Profile 分组完整且 ready、原卡与 generation 仍有效时，才在同一 Gate 锁内提交耐久操作对账。部分读取、换卡或 pending/quiescing 状态不能把未知写入结果清成成功。普通 SIM 判定要求本次全部 ISD-R AID 明确不存在、同一租约存在有效 USIM 身份、实际清理完成且 generation 一致；传输错误和 6982 不作为普通 SIM 证据，也不新增绕过逻辑。

读卡器切卡采用严格卡片返回值：错误状态不能被兼容幂等规则误当作启用成功。保留结构化错误、已有重试范围与安全诊断；没有重放额外写卡命令。模块既有幂等行为不变。

Go 的十个 reader eSIM 注册入口、专属 manager/adapter/cache、回调和旧刷新代码已退役，删除 7 个生产 Go 文件，生产文件数从 215 减至 208。模块 eSIM 及 serial/QMI 兼容服务继续保留。读卡器被动事实和旧借用客户端尚有 Go 消费者，是下一轮完整退休包，不能据本轮宣称全读卡器或整个后端已零 Go。

## 回归发现与修复

完整回归先后出现两项通话模式文件锁释放断言失败和证书测试超时。原锁断言没有实际错误细节，未据此断言 Busy 或确定首个失败进程的持锁者。独立 fork 对照确认，子进程继承同一打开文件描述时，仅关闭父描述符仍可能保留 flock。现使用不暴露 File 克隆能力的专用 guard，由最后一个实际 owner 显式解锁；共享、独占和物理 worker 的保留范围不变。新增确定性 dup 对照和最后 Arc 释放测试，两处原断言仍立即检查，不用重试掩盖失败。

证书阶段诊断确认 WrongName 用例在 macOS 系统校验区间超过原 8 秒夹具上限。夹具 watchdog 调整为 30 秒，生产证书策略和网络超时不变，实际 worker 清理的 2 秒证明保留。同时将原 80ms 无包观察改为等待客户端及必要的真实 worker 结束，再直接检查内核 UDP 队列；拒绝后零 SIM/EAP、错误类型和签名校验断言保留。相关测试修复及首次编译借用错误均有原始日志，未把最初失败改写成通过。

## 验证

- 完整 `scripts/test-migration-gates.sh` 单次修复后运行成功：Rust workspace **2,852 passed / 0 failed / 7 ignored**，Go vet/test、两组 Swift BLE 回归、13 组协议差分通过。4 个差分夹具随后显式执行，3 个转发互操作夹具本轮未执行。
- 定向读卡器网关 17 项、耐久对账 1 项、证书 7 项和通话模式相关 191 项通过；它们不代替完整门禁。
- 契约检查及 45 项扫描器自测通过：315 条契约，Rust HTTP owner 309 / Go owner 6，483 个审阅范围，88 条仍有 Go 归属或条件依赖（含 HEAD）。范围数不是完成率。
- 4,124 个门禁输入文件核对无漂移；最终仅追加文档和验收记录。Go 退出强制门禁仍按预期失败：208 个生产文件、6 类构建/运行/源码阻塞。

主要证据位于 `../build/reports/iteration-504-gate-summary.json`、`iteration-504-migration-gates-repaired.log`、`iteration-504-validation-repairs.md`、`iteration-504-integration-review.md`、`iteration-504-lock-contract-review.md`。旧进度账本的矛盾与去重要求见 `iteration-504-progress-ledger-audit.md`；不沿用 31.4% 或 38.31% 作为当前整体百分比。

## 后续

下一步完成读卡器被动事实、旧 Go 鉴权/借用客户端和对应关闭路径的整体退休，再推进串口原生生命周期、状态/SMS、APDU/eSIM、QMI 及最终 Go 构建/运行退出。真实设备、运营商互通、升级恢复与长稳验收仍未完成；全 Rust 目标保持进行中。
