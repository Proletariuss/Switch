# Iteration 461：N 待 Profile/Card 身份键归 Rust

N 待实体目录此前由 Go 根据读卡器枚举结果生成 `ProfileID` 和 `CardID`，再作为已确定的身份写入 Rust 持久目录。现在 Go 提交 reader、EID、ICCID 和展示元数据；Rust 在认证的内部目录写入中按原有 SHA-256 规则生成稳定 ID、校验重复/空白/异常字段、持久化目录并返回规范映射。Go 逐条验证 Rust 响应覆盖每个物理 Profile 后才发布到 runtime/recovery map；之后当前物理 ICCID 只能映射到这份 Rust 目录，不再由 Go 重算 ProfileID。

数据兼容性通过保留已有 ID 算法和哈希输入顺序确保，原有 durable catalog/desired keys 无需迁移。内部 HTTP 写入拒绝旧的调用方自带 ID 目录格式；仅直接 Store 旧格式读写逻辑继续服务历史数据 fixture。认证、请求大小上限、事件 ID 幂等、存储事务与公共 profile/card JSON 形状不变。返回映射不完整、重复、无法与实际 EID/ICCID 精确对应时 Go fail-closed，不启动或鉴权该 Profile。

定向回归：Go 多待身份映射/物理验证测试通过；Rust catalog discovery 单元测试和认证 HTTP 路由测试通过。最终 `scripts/test-migration-gates.sh` 通过，Rust workspace **1466 passed / 0 failed / 3 ignored**，Go 全仓 test/vet、Swift BLE manager 38组、2,722 carrier replay、USB/radio/HTTP method+HEAD 与 IMS/USSD/call/SMS parity 通过。迁移合同 `--diff` 通过：354 contracts（Rust293/Go61、51 HEAD aliases）、127 Swift HTTP calls、379 review scopes；`git diff --check` 通过。日志 [`iteration-461-migration-gates.log`](../../build/reports/iteration-461-migration-gates.log)，SHA-256 `495347bbf7d3b0ce60008c6d06d254b242ed42a7adc29fc58e64878696e1e9f5`。

结构退出审计仍为 **6 类 Go 依赖、266 个生产 Go 源文件**，详见 [`iteration-461-go-exit-audit.log`](../../build/reports/iteration-461-go-exit-audit.log)。此切片只转移稳定资源身份派生及目录接纳，不转移物理 Profile 切换、通知同步、AKA、IMS/SMS session 或恢复 executor；因此不增加全项目加权完成率，仍为 **18.045/47.5 = 37.99%**。Go `multiStandbyManager`、N 待多 Profile 读卡仲裁、原生 IMS production gate、设备/运营商和长稳验收仍未完成。未操作实体读卡器、号卡、SMS、电话或网络；9eSIM `6982` 继续排除。

本迭代 DMG 未重建。此前用户要求的 [Iteration 460 DMG](/Users/proletarius/Desktop/Switch/dist/Switch-3.15-macOS27-iteration460-rebuild-20261013.dmg) 的源码截止早于本迭代，不包含这些变更。
