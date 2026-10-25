# Iteration 505：读卡器 Go 客户端退出与 Rust 短信事实接管

2026-10-16。源码目录为 `/Users/StatTrak/Desktop/Switch_MAC/src`。本轮完整软件门禁通过；没有生成 DMG、变更版本号或操作真实号卡。最新已交付 3.42 / build 342 仍包含 Iteration 503。

## 实现

短信设备列表与内部设备选择的 reader 事实统一来自当前 Rust lifecycle、同代 IMS 会话和本地号码记录。被动 PCSC/BLE 也不再通过 Go 获取 reader 快照。兼容进程只提供模块事实，旧 reader 字段被有界丢弃，不能覆盖本地身份、号码、订阅或就绪状态。Go 不可用时继续展示本地读卡器，同时明确模块状态暂不可读取；没有 IMS 短信能力时不虚构 reader ready。

Go reader manager、身份/AKA、借用租约、PCSC/BLE 客户端、快照回调及旧关闭回调退出。删除 11 个 reader 生产文件，3 个 estkble 原实现转为明确的测试 oracle，新增 1 个必要的模块忙碌观察器。按相同口径，生产 Go 文件 **208→195**，生产代码净减 **3,225 行**，macOS daemon 实际导入包 **298→288**。共享模块 eSIM、AT/serial/QMI 代码保留；Go ClosePCSC 原来仅释放借用者，实际 Rust 物理关闭、worker join 和未确认关闭隔离继续保留。

15 个旧 reader-service、BLE transport、PCSC 和 operation 私有 HTTP 路由退出三个真实入口和 ownership；历史 adapter 只在测试中编译。typed Gate、原生 PCSC/BLE worker、lifecycle、公开 identity/authenticate 与 LPA 路径保留。模块 eSIM 仍需要 reader-business 进行只读忙碌检查，因此该入口保留 status-only：严格 producer 与字段校验，拒绝 acquire/transfer/release，保留失败关闭和退出阶段观察。

## 验证

- 完整修复后 `scripts/test-migration-gates.sh` 一次运行成功：Rust **2,861 passed / 0 failed / 7 ignored**；Go vet/test、两组 Swift BLE 和 13 组协议差分通过。4 个差分夹具随后显式执行，3 个转发互操作夹具本轮未执行。
- 非测试 `cargo check -p proletariusd --bins` 通过，验证 cfg(test) 隔离后的生产编译；4,112 个门禁输入文件核对无漂移。
- 720 种真实 HTTP 组合验证旧接口为 404、零 Go 转发、reader selection 不变。物理 owner 不可达由入口早退源码证明，该矩阵没有另设物理 factory trap。
- 定向 SMS devices 27 项、只读 business 2 项及现有实际 Service/IKE/IMS SMS 集成夹具通过。验证原生订阅/号码/ready、双设备选择冲突和真实会话停止后旧事实不能恢复 ready。模拟网络与 SIM 夹具不等于真机运营商验证。
- 契约一致性、45 项扫描器及 4 项 Go-exit 检查器自测通过：**300 条契约，Rust HTTP owner 294 / Go owner 6，472 个审阅范围，76 条仍有 Go 归属或条件依赖**。54 个 HEAD 别名与 127 个 Swift HTTP 调用点保持不变。
- Rust-only 与 Go-free 强制门禁仍按预期失败：195 个生产 Go 文件，6 类构建/运行/源码退出阻塞。没有用接口或文件计数折算整体百分比。

首次定向与全量回归失败均保留日志。修复包括新增 HTTP 夹具对 macOS ENOTCONN 的正常关闭处理，以及三处仍把退休接口视为活跃接口的测试预期。生产数据、鉴权、原生提交/存储失败和物理清理断言没有被移除；详见 `../build/reports/iteration-505-validation-repairs.md`。

验收汇总为 `../build/reports/iteration-505-gate-summary.json`，完整日志为 `iteration-505-migration-gates-repaired.log`。三个独立审查与根线程集成审查均在同一 reports 目录。notes I/O 窗口及 active IMS/延迟 Go 的完整确定性调度组合尚无全覆盖，真实设备、运营商和长稳仍待验收。

## 当前剩余工作

继续完成串口原生生命周期与事件/状态/SMS、APDU/eSIM、QMI 和其他模块兼容路径，随后退出 Go 构建、嵌入、启动及分发。读卡器 eSIM 变更对非原生模块的 `/api/esim/operation` 条件忙碌观察仍保留，不能宣称整个读卡器相关调用链已经与所有 Go 依赖完全隔离。

全 Rust 项目目标仍进行中。旧 31.4% / 38.31% 账本存在归属与余额问题，不作为当前总完成率；只有必需能力、零 Go 和最终验收全部完成，才标记 100%。
