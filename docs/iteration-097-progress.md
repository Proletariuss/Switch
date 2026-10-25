# Iteration 097：公开 HTTP 方法兼容与差分门禁

日期：2026-10-05。全 Rust 后端目标继续执行。上一轮完成 RF 接管并留下明确的 HEAD/405 兼容缺口；本轮实现并验证该方法边界，没有操作设备、真实卡片或生产短信，没有生成 DMG。

## 行为与范围

新增 `public_http.rs` 的 57 项公开路径策略表；方法集合与独立提取的实际 Rust 分发、同路径 Go 注册交叉检查。只有表中明确的公开 GET 可以将 HEAD 交给相同读取处理器执行一次。HEAD继承该GET既有行为；例如eSIM通知列表仍可能自动投递并确认通知，本轮不将其定义为无副作用查询。鉴权与内外接口分流先于方法处理，内部接口及退出清理白名单不扩大。

- 修复已迁 GET 的 HEAD 落入 Go fallback：flight-mode、短信列表、通话记录、reader eSIM 读取以及原生读取端点均由相应读取 owner 处理。HEAD 响应不发送正文，保留该表示的 Content-Length/Content-Type；已有 Go HEAD 的长度不被空本地正文改成零。204/304 或原始无长度响应不凭空增加 Content-Length。
- 仅对有冻结 Go 注册依据的 21 个旧公开路径恢复原 405、字典序 Allow、文本正文及安全响应头。方法集合包含 GET 隐式 HEAD，并保留同一路径 PATCH/DELETE 等所有可用方法。错误方法在存储/硬件/后端可用性检查前拒绝，不执行原业务。
- Go 专属和未知路径保留原方法、查询参数及转发行为；某些 Go 业务本来拒绝 HEAD，不能将其全局改为 GET。后端恢复期间 HEAD 按读取等待，写入仍不自动重放。
- 原生接口原有错误语义不统一改写：短信导出创建入口 HEAD 仍为 400，不创建导出；已有 token 的 HEAD 读取当前分块但不删除、不移动偏移、不刷新寿命，非法 offset 为 400，已删除 token 为 410。其余原生错误方法仍由原 handler/fallback 决定。

本轮覆盖规范形式的路径及查询参数。URL 清理重定向、转义段、尾斜线与子树匹配仍须单独验收；当前 Go 1.26.3 清理路径使用 307，不能按旧版本印象改为 301。此项未完成，不宣称整个 HTTP 路由兼容性已验收。

## 验证

新增 5 项真实 Unix socket 回归，逐字节检查线上响应，避免 HEAD 客户端自动丢弃正文掩盖服务器错误；覆盖认证、私有接口、退出、无 Go 读取、405、Go 原 HEAD 单次转发及导出生命周期。另有 4 项策略/响应元数据测试。

独立 Go oracle 使用冻结的 24 个原路由注册（21 条路径），通过真实标准库 HTTP 服务导出 209 个样本。Rust 对其中 **197 个原生响应**逐项比较状态、Allow、Content-Type、Content-Length、正文及处理次数；其余 **12 个样本**确认仍交给旧 fallback，不冒称 Rust 已处理这些业务。两个仍在 Go 的真实业务 handler 的 HEAD 拒绝行为另作回归。Go 定向 race **2 个顶层 / 213 个含子测试通过，0 失败、0 跳过**。

完整 `scripts/test-migration-gates.sh` exit 0：Rust workspace **1105 通过、0 失败、14 个默认忽略**，其中 daemon **622 通过、0 失败、3 个默认忽略**。新增 HTTP 差分已加入永久门禁，在专用步骤实际运行；已有 USB/RF、运营商、SDP/AMR 差分及 Go vet/兼容测试继续通过。PCMA 只保留历史离线夹具，不作为主机通话选项。本轮没有完整 App 构建或实机验收；两个既有 native VoWiFi dead-code warning 仍在。

契约清单现为 **240 项（Rust 142、Go 98）**，其中新增的 **20 项是 HEAD 别名**；扣除派生项仍为 Rust 122 / Go 98。Swift调用点127，审阅scope164。检查工具通过41项自测，新增外部测试模块引用图识别与公开方法表校验；15个只被测试引用的源码不再被当作生产API引用，共用生产引用则保留。

日志均在 `Switch/build/optimization/`：

- `2026-10-05-http-method-check.log`
- `2026-10-05-http-method-tests.log`
- `2026-10-05-http-method-parity.log`
- `2026-10-05-public-http-method-oracle-race.jsonl`
- `2026-10-05-public-http-method-parity.json`
- `2026-10-05-http-method-migration-gates.log`
- `2026-10-05-http-method-results.json`（本轮汇总）
- `2026-10-05-http-method-contract-summary.json` / `2026-10-05-http-method-contract-runner.json`（契约计数、四项命令结果及日志）

最终契约写入与一致性检查均 exit 0，41 项工具自测通过；Rust-only 按预期 exit 1，仅因默认 Go fallback 和 151 条 Go 归属/依赖契约而拒绝。依赖条数从 144 增加到 151 是 7 个 HEAD 别名继承原 GET 依赖，不是新增 Go 业务。

## 进度口径

整体已验收估算仍为 **24.9%：11.825 / 47.5 计划人周，误差约 ±3 个百分点**。本轮补齐迁移兼容缺口，不把 HEAD 别名或路由数量重复记为业务迁移增量。P2A 61%、P2B 47%、P5 0% 保持；Go 业务和默认 fallback 尚未退出，不能标记全 Rust 完成。

语音目标保持 **EVS 优先、AMR 回退，不使用 PCMA**；当前 AMR-NB mode 7，EVS/AMR-WB 尚未实现，新增成本待试点重估。9eSIM `6982` 继续排除。后续仍须完成剩余硬件/卡片业务、IMS/VoWiFi/N待、媒体、数据和实机矩阵，以及最终零 Go 构建/运行依赖。见[实施仪表](optimization/2026-10-04-all-rust-execution.md)与[全 Rust 方案](optimization/2026-10-04-all-rust-backend-plan.md)。

## 下一工作包：PC/SC 实体会话与 APDU owner

已按当前源码核对下一接管边界：Go reader_manager.go 仍持有 reader/generation、authMu 和 persistentCCID，reader_session_lease.go 持有 token 与续期；Rust reader_session_lease.rs 仍反向借用 Go 准入，而 LPA/AKA 还有多处按操作 PcscSession::connect。这属于实质未迁能力，不能只搬计时器就算完成。

下一步让一个 Rust reader actor 统一持有 PC/SC 句柄、session epoch、完整 APDU 事务、逻辑通道、复位及实际关闭等待；Go 的 PCSC CCID/USIM/AKA/N待入口改为同会话代理，Rust LPA/AKA 也统一接入该 owner。保留 generic CCID 共享句柄、ESTK 原允许的复位及关闭失败语义，不扩大冷复位范围。BLE 保持独立边界并明确未迁。

离线门禁包括同reader零重叠、身份读取到AUTH在同句柄、取消后真正I/O完成才释放、ACK丢失/先释放后获取不复活、旧会话结果不污染替换后设备，以及 fake PCSC 证明所有 connect/transmit/reset/close 只从 actor 发出。现有 reader manager/channel/card-service/auth-identity 测试可复用；真实卡片、插拔与运营商验收仍单独记录。
