# Iteration 687 — 3.50/350 来源与归档边界复核

i686 的 549 条选择性来源快照、QMI 静态 trace 与审阅记录保持原始字节，作为本轮不可变直接前驱。i687 对全部 549 条旧条目重新计算当前 SHA-256，发现 10 个既有文件变化；账本检查器与单测推进后，最终后继共有 12 个已审阅变化、0 个新增条目，仍为 549 条。

当前候选版本为 **3.50/build 350**，对应 `VERSION`、`BUILD_NUMBER`、`project.yml`、Rust 工作区清单及锁文件。Rust eSIM intent ledger 在容量满、持久记录损坏及同一请求并发争用时保留不重复物理派发的边界，相关局部测试由业务修改方验证；这不代表真实读卡器或运营商验收。历史 **3.48** 发行清单的 successor pin 指向版本归档路径，发行脚本新增 DMG/清单归档的原子发布和失败清理，隔离 fake flow 测试覆盖归档故障。本轮没有运行真实打包。契约清点仍为 304/304 个 Rust HTTP owner、Go owner 0，接口身份与 owner 不变。

QMI 九个源文件及 57 个精确锚点未改；private `.in` 鉴权、public/`.rs` 对 `/api/internal` 的拒绝结论不变。QMI trace 只推进当前来源快照和历史审阅 pin。runtime map 中 eSIM intent `execute` 的精确锚点随新增守卫从第 438 行重绑至第 445 行，来源哈希同步刷新；节点仍不计作已验收单元。正式账本仍为 49 父项、115 叶、21 场景类，未授予新功能 PASS，整体分母未冻结，P0–P6 尚未完成。正式冷验收仍需同源码完整 prepare、execute、verify 收据；当前 scope-wide `unknown` eSIM intent 尚无明确的用户恢复路径，属于后续独立修复和验收范围。版本归档保护和局部测试不能替代可用 App、设备或产品验收。
