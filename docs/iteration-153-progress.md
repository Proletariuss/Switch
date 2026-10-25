# Iteration 153：N待入站短信与读卡器 IMS 身份的 Rust owner

## 完成内容

- Go N待 IMS 回调不再计算 `mt_` 网络去重键，也不再组装持久化入站记录。它只将 profile ID、原始 RPDU/TPDU、发送方/正文、时间戳、解码/状态报告和拼接元数据交给已认证的 Rust 存储路由。
- Rust `inbound-event` action 按现有 `SHA256("mt\0profile\0rpdu")` 前 16 字节规则生成兼容去重 ID，采用原有耐久写入/收件箱路径；保留网络重试去重、profile 隔离、状态报告与长短信行为。若拼接段冲突，Rust 回滚本次分段状态并将原始事件以未解码记录安全保存，避免错误合并。事件仍经原 Go IMS session 生命周期处理，不代表整个 IMS 栈已迁 Rust。
- Rust `reader_device_identity` 现在拥有共用 `imei.json` 的首次身份生成与稳定读取。首次生成在线程/进程锁内使用系统随机数，保留旧 353936 TAC 格式和 Luhn 校验位，原子写入原配置文件。私有 POST resolver 只在 main 内部 token 校验后可达，daemon shutdown 拒绝新请求。Go VoWiFi adapter 启动 IMS session 时向 Rust 查询，owner 不可用时 fail closed；Go 文件生成逻辑只保留显式 Go-only 兼容模式。
- 迁移清单更新为 285 条 method/path（Rust 213、Go 72，含 34 个 HEAD alias）、127 个 Swift 调用点和 251 个审阅范围。Rust component manifest 更新为 7/18 个 Go-free 组件；这是组件覆盖口径，不等于总体加权进度。

## 验证

- `scripts/test-migration-gates.sh`：通过；涵盖 Rust workspace、Go 全仓 vet/test、2,722 条运营商模板回放，以及 USB 状态、RF 策略、HTTP/HEAD、codec 和 SDP parity。
- Rust workspace：1,394 passed、6 ignored；其中 `proletariusd` 901 passed、3 ignored。Rustfmt 通过。
- Rust 测试覆盖 N 待重试去重、Profile 隔离、多段完成/冲突回退、私有持久化路径；身份 owner 测试覆盖并发首次生成、公开配置/清除后的解析；main 路由测试覆盖未认证拒绝、仅 POST、成功持久化和 shutdown 拒绝。
- Go 全仓 `go test ./...` 和 `go vet ./...` 通过；IMS owner RPC adapter 和 N 待原始事件 payload 定向 race 测试通过。
- 迁移契约 `--diff` 与 43 项工具自测通过，统计为 285/213/72/127/251。Rust-only 门禁继续按预期失败：默认 Go fallback 仍存在，145 条路径仍归 Go 或有 Go 依赖。

## 进度与边界

按计划权重计 P2B +0.025 人周、P4 +0.025 人周；累计 13.370/47.5 计划人周，约 28.1%（估计误差 ±3 个百分点）。组件清单的 Rust ownership 覆盖为 7/18（38%），不要与工期加权百分比混用。

本轮没有构建 App/DMG，没有连接读卡器、操作 SIM 或触发运营商网络；IMS 在线注册、短信实际投递、多 Profile 并行及掉线恢复仍未实机验证。9eSIM `6982` 专项继续排除。呼叫香港号码问题按用户更正停止项目侧排查。

下一步继续处理 Rust IMS socket/SIP dispatch 与会话 owner、VoWiFi 完整生命周期、N 待多 Profile 恢复，以及正常 eSIM 下载/LPA 余项；最终仍须通过零 Go 构建/运行、数据升级/恢复和实机验收才可 100%。
