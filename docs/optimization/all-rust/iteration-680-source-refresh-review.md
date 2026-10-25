# Iteration 680 — 当前来源与私有路由审阅

3.48/build348 的 i677 完整软件冷构建、App 和三轮软件审议已在其不可变来源切片验收；本条只审阅之后的源码变化，不继承该通过结论到当前源码，也不宣称产品完成。

## 当前变更

- `main.rs` 的公开监听器与 `.rs` 同走 `handle_request_inner`，现在所有 `/api/internal/` 路径均在令牌处理前返回 `route_not_found`。仅独立 `.in` 的 `handle_internal_request` 保留私有令牌鉴权和 QMI 路由。先前 i677 QMI 静态审阅的三处公开网关匹配、鉴权、分发锚点已失效；本轮以一处完整的公开拒绝锚点替代，旧 i677 审阅作为历史证据不改写。i680 仍只是静态路由、owner、format、exit 有限审阅，未验证 QMI 硬件和运营商。
- `public_http/tests.rs` 增加公开及 `.rs` 令牌组合拒绝的负例，以及私有处理器合成正例。当前调试 daemon 三接口进程报告 18/18，但其内部令牌未导出；完整 P1-5 和关闭期间保留请求仍未通过实进程验收。
- `clean-generated.rb` 限定只可清理有审计标记的历史冷构建 Swift 缓存，并检查编译进程；对应 `test-clean-generated.rb` 本轮首次纳入 531 条选择性来源目录。清理收据仅证明列明缓存已移除，不说明其他路径可删。
- `test-migration-gates.sh` 加入只读 current-baseline successor checker 及其负测；inactive 的 P0-1 v3 候选尚未成为活动账本，不经该门禁自动授 PASS。
- `check-acceptance-ledger.py` 与负测切换到 i680 当前快照和 57 锚点 QMI 静态审阅，i677 及更早祖先精确原始字节继续锁定。

304 个接口 ID、方法、路径、owner 与迁移归属保持；对比 i677 冷胶囊 API catalog，132 条记录只改变 `sources` 字段（其中 3 条同步更正私有入口函数与说明），没有新增或删除接口。entry map 中 356 个当前 `main.rs` source-ref 均按原片段在当前源码唯一定位；runtime map 中 59 个历史节点与 8 个当前替代节点同样唯一重绑。原始历史锚点未回写。完整当前 source-identity 域冷比较和全量门禁仍待同源重跑。

形式验收仍为 115 叶、0 PASS、5 PARTIAL、110 NOT_REVIEWED，产品分母未冻结。P0 未闭合，P1–P4 验收中，P5/P6 开放；硬件、运营商、实网、物理音频及长期运行不因本静态审阅升级。最终 DMG 仅在完整验收后按用户要求生成。
