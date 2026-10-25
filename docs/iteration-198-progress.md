# Iteration 198：本地短信单条删除迁入 Rust

日期：2026-10-09。范围限定在 `/Users/proletarius/Desktop/Switch`。

Rust 现在拥有监督模式 `POST /api/sms/delete` 的公开 dispatch 和本地删除策略。请求体继续受旧接口 1 MiB 上限约束；Rust使用严格字段解析，支持稳定短信ID，也保留旧客户端按 sender、content 和时间戳近邻匹配的选择方式。删除前冻结用户可见列表与Go易变缓存分区，只把与选中稳定ID完全匹配的冻结项目交给认证适配器同步；Rust随后更新耐久归档和USB预览。

物理SM/ME分支仍由Go的原有 `deleteSMS` 工作流处理。Rust只在原有 `index > 0 && storage != LOCAL` 条件成立时转交；Go仍校验刚读取的存储快照、ICCID/session绑定的删除凭据、模块存储索引和硬件租约，再按旧规则协调来源别名与响应。显式Go兼容模式仍保留原公开handler。此项完成本地业务迁移，不代表短信物理删除端到端脱离Go。

验证结果：

- Rust workspace：**1,090 passed / 3 ignored**；Go全仓测试与 `go vet` 通过。
- 独立Go/Rust进程集成：**32项通过**，包括本地短信删除后Go缓存同步清理和短信不重新出现。
- 载波回放：**2,722项**；USB状态、radio、HTTP method/HEAD、SDP及AMR parity通过。
- 迁移契约扫描：**331条**（Rust 266、Go 65，含48个HEAD alias）、127个Swift HTTP调用点、298个审阅范围；43项扫描器负例测试、`--diff`和 `git diff --check` 通过。
- Rust-only退出检查仍失败：默认Go fallback存在，**176条**路由仍有Go owner或Go运行依赖；本路由本身仍依赖Go缓存同步和物理SM/ME事务。
- 没有操作实体读卡器、SIM或运营商网络；没有构建App或DMG。

进度按原计划中值保守给P2A增加 **0.025人周等值**：累计 **14.720/47.5 = 31.0%**（估算误差约±3个百分点）。P2A为4.085/5.5（74.3%），P2B 5.375/8.0（67.2%），P3 1.640/8.0（20.5%），P4 0.750/4.5（16.7%）。

证据日志：`build/reports/migration-gates-iteration198.log`、`build/reports/iteration198-contract-selftests.log`、`/Users/proletarius/Desktop/Switch/build/reports/multistandby-iteration198.log`。
