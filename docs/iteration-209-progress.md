# Iteration 209：普通 USB 模块 eSIM 下载/确认执行迁入 Rust

日期：2026-10-09。继续 Switch 全 Rust 后端重构，只在 Switch 源码目录内工作。没有连接实体读卡器/SIM，没有下载真实 eSIM profile，没有访问运营商、发送短信或呼叫，也没有构建 App/DMG。9eSIM `6982` 继续按用户要求不处理。

## 实现

- Rust 现在处理普通 USB 模块 `POST /api/esim/download` 的请求校验、durable operation/progress、LPA安装、成功结果和下载记录，并将 `POST /api/esim/download/confirm` 接到当前 Rust 下载确认操作。
- 复用 Rust reader LPA/ES10c installer、profile-size确认流程、module-radio eSIM reservation与USB AT transaction owner；在独占事务内管理CCHO/CGLA/CCHC eUICC通道。
- 仅在modem明确拒绝CCHO且任何APDU尚未发送时允许有限备用AID尝试。开始LPA/APDU后不重放安装；通道关闭或reservation释放不确定时记录清理错误并停止。
- 保留模块Profile切换前停止VoWiFi并确认会话清理/恢复的安全语义：Rust调用新的认证Go内部适配器。Go operation observer、非USB兼容回退与部分缓存边界仍在；本轮不声称完整eSIM生命周期或整个后端已Go-free。
- `6982`继续失败关闭，不绕过运营商卡鉴权或尝试复用受限密钥。

## 验证

- Rust下载请求严格解码、活动确认操作、eSIM owner边界和普通模块USB/APDU路径定向测试通过。
- Go停止模块VoWiFi适配器的内部令牌认证、错误字段和未知字段测试通过。
- Rust migration-contract checker：43项自测通过；常规源码清单核对通过：335个method/path契约（Rust274、Go61，含49个GET派生HEAD），127个Swift调用点，326个review scopes。
- 完整 `RUST_TEST_THREADS=1 bash scripts/test-migration-gates.sh` 已通过：Rust workspace **1,116 passed / 3 ignored**；Go全仓 `go test`/`go vet`、2,722项运营商模板回放、USB/radio/HTTP method+HEAD/codec/SDP parity通过。
- `git diff --check`通过。`--require-rust-only`仍因默认Go fallback及177条Go owner/dependency路由失败；后端退出审计仍列出7类Go依赖与255个生产Go源文件。上述为已知未完成项，不是此次功能门禁失败。
- 仍有此前的 `native_vowifi_session.rs` dead-code与重复 `-lusb-1.0` 链接器警告。

## 进度与剩余工作

保守计入 eSIM/LPA **+0.050计划人周等值**：由 **1.225/5.5** 调整到 **1.275/5.5（23.18%）**。总进度由 **14.870/47.5（31.3%）**调整到 **14.920/47.5≈31.4%**，进度权重误差约±3个百分点。剩余工期规划约 **210–380个自然日，中值约290天**，本轮不改变区间。

尚未完成：Go operation observer和VoWiFi停止适配器退出、非USB模块fallback、完整Profile状态/概览无Go回读、其余eSIM lifecycle、IMS/VoWiFi keyed session actor、短信与语音端到端生命周期、Go-free构建运行、真实设备与运营商验收，以及长期稳定性观察。
