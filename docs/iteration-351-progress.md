# Iteration 351：N待恢复选择策略迁入 Rust

日期：2026-10-11。范围：`/Users/proletarius/Desktop/Switch/src`。本轮无实体号卡操作、短信发送或 DMG 构建。

## 变更

N待启动恢复过去由 Go 从 Rust 返回的目录中排序、过滤 `keep_online` 与 tombstone，并选择首张实体卡。现将这段业务选择策略迁入 Rust：Rust 基于持久化意图、Profile 目录状态和本轮物理扫描得到的 Profile ID 清单，产出应恢复的稳定 ID 集合。Go 仅验证 Rust 返回的集合确实属于当前物理盘点、处于同一张卡且顺序唯一，再将 ID 绑定到当前物理对象并调用既有 session 启动流程。

该边界保留物理盘点和会话建立在 Go 的现状；RPC 异常或非法响应会失败关闭，不会静默恢复错误 Profile。新增内部私有查询 `POST /api/internal/multistandby/store/recovery-selection`，并加入 API 动态路由审阅、源码指纹和契约清单。

## 验证

- Go 恢复集合绑定/非法响应测试通过。
- Rust 恢复集合策略与内部 HTTP 鉴权/输入验证测试通过。
- 全量迁移门禁通过：Rust workspace 1,889 passed / 6 ignored；daemon 1,354 passed / 3 ignored；Go 全包测试及 vet、跨语言差分和契约检查通过。
- 当前契约清单：343 method/path（Rust 283 / Go 60）、127 Swift 调用点、353 审阅范围；契约工具 43 项测试通过。
- 门禁日志：[iteration-351-migration-gates.log](../build/optimization/iteration-351-migration-gates.log)

## 进度

按既定 47.5 计划人周加权口径，本轮仅将生产选择策略迁移计入 P4 +0.025 人周等值；不把物理盘点、Profile session 启动或 IMS/SIP 生命周期算作 Rust 接管。累计 16.020 / 47.5 = 33.7263%，展示为 33.7%（两位小数 33.73%）。进度估算误差约 ±3 个百分点；全 Rust 后端目标仍未完成。
