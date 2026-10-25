# Iteration 334：Rust 权威读卡器身份快照与代次投影

2026-10-10。变更仅在 `/Users/proletarius/Desktop/Switch` 内。未连接或操作实体读卡器/SIM，未发送短信或拨号，未生成 DMG。

## 变更

- Rust gate 现持有 reader 名称与 selection generation 绑定的 USIM/ISIM metadata 快照；发布时校验 ICCID 和当前 generation。切卡、身份代次改变、断开、Go producer 退出都会清除旧快照。
- Go 在身份生命周期回执提交后，向认证 Rust 内部接口发布新快照；只有收到 reader/generation 完全匹配的成功回执后，才更新 Go 兼容缓存。发布失败会保持 Go cache 为空，不暴露旧卡数据。
- Rust 公开读卡器信息投影使用 Rust generation-bound 快照，并同步更新 `has_usim_info`。Go 返回的数据只继续提供兼容状态和 eSIM profile 名称等事实，不再作为公开 USIM/ISIM 快照的权威来源。
- 为快照接口补充内部 route ownership 与 64 KiB 请求体准入。

## 验证

- Rust 定向测试确认：当前 generation 可发布/读取；卡片身份切换会使旧快照失效；旧 generation 不能回填；新 generation 可重新发布。
- Go reader lifecycle 刷新定向回归通过，确认 Rust receipt 之前不发布新缓存、回执丢失或 Rust snapshot 发布失败时均不暴露旧身份。日志：[reader lifecycle targeted tests](../build/optimization/iteration-334-reader-lifecycle-targeted.log)。
- 完整 `scripts/test-migration-gates.sh` 通过：Rust daemon **1,320 passed / 3 ignored**，其余 Rust workspace crate 测试通过；Go 全包与 vet、Swift BLE bridge/manager（38 组事件序列）、2,722 carrier replay，以及 USB/radio/HTTP/SDP/AMR/MESSAGE 差分检查通过；最终标记 `migration gates passed`。日志：[Iteration 334 migration gates](../build/optimization/iteration-334-migration-gates.log)。
- `git diff --check` 通过。未做真机 reader/SIM、运营商注册或 App/DMG 验收。

## 进度

按 P2B 增计 **+0.050 计划人周等值**：Rust 接管公开 USIM/ISIM 身份快照的代次权威存储和读取，Go 兼容副本仅在 Rust 确认后更新。累计 **15.570 / 47.5 ≈ 32.8%**，估算误差约 ±3 个百分点。Go 业务缓存、模块身份路径、完整 reader 物理生命周期和实机验证仍未迁出/验收；本迭代不代表读卡器整体或全后端改造完成。

下一步迁移 module-backed identity source，并将 reader connect/identity/switch 与物理生命周期适配合并到 Rust owner；随后继续迁移 BLE LPA/AKA、IMS/N待生产会话和媒体生命周期，最终完成零 Go 构建/运行/嵌入验收。
