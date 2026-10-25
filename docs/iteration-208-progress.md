# Iteration 208：普通 USB 模块 eSIM Profile 切换接入 Rust

日期：2026-10-09。继续推进 Switch 全 Rust 后端；仅在 Switch 源码目录内工作。没有连接实体读卡器/SIM，没有下载或切换真实 Profile，没有访问运营商，没有发送短信或拨号，没有构建 App/DMG。9eSIM `6982` 按既定要求不处理任何绕过方案。

## 实现

- 普通 USB 模块 `POST /api/esim/switch` 现在由 Rust 处理；更新 route ownership、main dispatch 和公开 HTTP method policy，冻结旧 Go mux method oracle 也移除该已迁移路径。
- 请求经过有界解码和 ICCID 校验；Rust 通过既有 Go operation observer fail-closed 检查并发操作，再取得共享 module-radio eSIM reservation 与耐久 Rust eSIM 写 permit。
- Profile APDU worker 在 HTTP 客户端取消后仍继续完成状态收尾，permit 覆盖 APDU 执行、读回与终态 journal。成功路径最多执行一次 EnableProfile；模糊响应后换新逻辑通道只读验证，不重复 EnableProfile。
- APDU 完成后由 Rust 限时监控模块 ICCID；必要时最多刷新一次，并使用共享 module-radio reservation action。90秒内等待调制解调器切换结果，之后记录成功或不确定/失败终态，不重启模块。
- `6982`、profile list 读取失败及通道清理异常均 fail-closed，不探测备用 AID、不绕过鉴权。非 USB 路径保持 Go 兼容回退，活动 operation observer 仍调用 Go，故完整 eSIM 生命周期尚未脱离 Go。

## 验证

- module eSIM fake AT/APDU 测试：12项通过，覆盖正常切换、启用响应不确定后的只读回查且不重发、`6982` 停止探测与 ICCID 格式校验。
- `RUST_TEST_THREADS=1 bash scripts/test-migration-gates.sh` 完整门禁通过：Rust workspace **1,115 passed / 3 ignored**；Go 全仓 `go test`/`go vet`、2,722项 carrier 回放、USB/radio/HTTP method+HEAD/codec/SDP parity均通过，冻结 Go HTTP method/HEAD 测试通过。
- `python3 scripts/tests/test_rust_migration_contracts.py`：43项通过；`python3 scripts/check-rust-migration-contracts.py --root .` 通过并与登记清单一致；契约清单334条（Rust272/Go62，含49个HEAD alias）、127个Swift调用点、323个review scopes。
- Rustfmt 与 `git diff --check` 通过。保留既有 `native_vowifi_session.rs` 两条 dead-code warning 和重复 `-lusb-1.0` 链接 warning。
- Rust-only退出门禁仍失败：默认Go fallback尚未删除，176条路由仍含Go owner或依赖。独立后端退出审计仍识别7类Go构建/运行/源码依赖和254个生产Go源文件。

## 进度与剩余工作

保守计入 eSIM/LPA **+0.025计划人周等值**，由 **1.200/5.5** 提升至 **1.225/5.5（22.27%）**；总体由 **14.845/47.5** 提升至 **14.870/47.5≈31.3%**，误差约±3个百分点。P2A **4.110/5.5**、P2B **5.400/8.0**、P3 **1.640/8.0**、P4 **0.750/4.5**维持不变。剩余工期区间继续按约 **210–380个自然日，中值约290天**规划，本轮不足以改变区间。

Go operation observer、非USB兼容回退、eSIM状态和缓存兼容边界、完整LPA生命周期、IMS/VoWiFi生产session actor、短信与语音业务owner、Go-free构建/运行，以及实体设备/运营商/长期稳定性验收仍未完成。此轮实现属于普通USB Profile 切换执行边界迁移，不等于完整 eSIM 或全项目 Rust 重构完成。
