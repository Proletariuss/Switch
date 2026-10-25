# Switch 3.37（构建 337）

2026-10-14。按用户要求，从 Iteration 498 最新源码构建。

[下载 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.37-macOS27.dmg)。Apple Silicon，macOS 27.0+。

- 普通读卡器公开 VoWiFi 启停、状态、短信和语音接入 Rust Service；旧 Go N待管理器及 19 条内部契约退役。
- 补全 BLE 连接代次和物理关闭回执校验，修复退出清理误等 Go，以及 Go 子进程退出时误恢复仍在进行的原生短信。
- 修复运营商模板显式覆盖优先级与可执行算法回退；2,730 条有效配置及算法差分通过。这不是所有运营商实网验收。
- 发行前全量门禁：Rust workspace 2,743 passed / 0 failed / 7 ignored，其中 4 个 ignored 已在后续差分执行通过；Go vet/test、Swift BLE 39 组与关闭证明、13 组差分、49 项扫描工具自测通过。3 个互操作夹具不在本门禁执行范围。发行版本修改未改变业务源码或外部依赖。
- 3.37 / 337 Release 构建通过，320 条接口契约核验通过。4 个 Mach-O 均为 arm64，13 个许可文件完整；严格签名、DMG 校验、只读挂载与构建 App 文件逐一一致性核验通过。两个后端的装载检查在数据目录校验阶段退出，没有初始化硬件。
- 本机 ad-hoc 签名，未公证；未替换已安装 App，未做本版实卡、通话、短信、长稳或运营商验收。模块 VoWiFi 和其他剩余路径仍依赖 Go，全 Rust 目标未完成。

SHA-256：`4dfbbd6e5681c320a86ff7b10696a42ede906e0d9ebff5b6de4297cec139feb5`

大小：55,581,431 字节。

源码变更：[Iteration 498](iteration-498-progress.md)。构建及验证证据：`../build/reports/release-3.37/`。
