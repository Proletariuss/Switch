# Switch 3.36（构建 336）

2026-10-14。按用户要求，从 Iteration 497 当前源码构建。

[下载 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.36-macOS27.dmg)。Apple Silicon，macOS 27.0+。

- 包含公开 N待 Rust Facade：卡片/Profile清单、启停与重试、短信入队及维护/重试/恢复调度；六条旧 Go N待运行接口已退役。
- 全部前端版本为 3.36 / 336，Rust workspace 及23个本地包为 3.36.0；外部依赖没有更新。
- Release 构建、141项原生N待组合及336条接口契约核验通过。业务源码与上一轮通过2728项Rust、Go、Swift和跨语言检查的代码一致；本次只重跑发行相关检查及N待组合，不重复宣称全量新测试。
- 四个 Mach-O 均为 arm64，13个许可文件完整；严格签名、DMG校验、只读挂载与构建App文件逐一一致性核验通过。两个后端的装载检查在数据目录校验阶段退出，不初始化硬件。
- 本机 ad-hoc 签名，未公证。未安装替换现有App，未做本版实卡、通话、短信或运营商验证。Go 仍是运行依赖，BLE无物理关闭证明时仍保持未确认，全Rust目标未完成。

SHA-256：`b4ed27f9a144f766b9519c5df5b4055df15efbb677c2ba492d48dcff3771c2fd`

大小：55,623,564 字节。

构建及验证证据：`../build/reports/release-3.36/`，包括源码指纹、版本同步、构建日志、组合测试、签名、装载、镜像与文件一致性报告。
