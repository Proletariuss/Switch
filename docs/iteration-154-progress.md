# Iteration 154：IMS 入站 RP-ACK delivery-report 帧迁入 Rust

## 完成内容

- 在现有认证私有 `POST /api/internal/ims/sms-transactions` 中增加 `prepare_delivery_report` action；未增加 method/path route。Go production coordinator 通过原认证 UDS 传入目标 IP-SM-GW URI、`In-Reply-To`（原入站 Call-ID）、当前注册的 IMS 身份与 Route、Security-Verify/PANI、Port-C 和 RP reference。
- Rust `proletarius-vowifi` 构造 delivery-report SIP MESSAGE 帧：验证 URI/headers，保留 TCP Via、注册身份、Route、安全头、`application/vnd.3gpp.sms` 与 binary encoding，并调用既有 SMS codec 生成 RP-ACK body `[0x02, reference]`。不带只适用于 originating MESSAGE 的 `Accept-Contact` 和 `Request-Disposition`。
- Rust daemon transaction actor 为 delivery report 生成 Call-ID/tag/branch，并让它与普通外发 MESSAGE 共用每 IMS session 的 CSeq high-water owner；仅在有效 frame 生成后提交新的 CSeq。
- Go 入站 MESSAGE handler 原有行为保持：先返回 SIP 200，随后解析 RP-MT-DATA并调用持久化 callback；只有 callback 成功才继续发另一个 SIP MESSAGE，其 `In-Reply-To` 对应原 Call-ID，body 为 RP-ACK。Go把 Rust返回帧写当前注册流一次，并继续在Go response loop中匹配Call-ID/CSeq/方法及SIP final response。
- 加入 Rust frame/actor、Go coordinator bridge 和生产 `sendIncomingRPAck` 路径的合约回归；验证目标 URI、二进制参考值、In-Reply-To、注册身份、Route、安全头及共享 CSeq。

## 协议语义与所有权边界

这里的 RP-ACK 是 SMS RPDU 层的 RP-ACK（`02 || reference`），装在一条独立的 SIP `MESSAGE` 请求中；它不是 SIP `200 OK`。Go在处理原始入站SIP `MESSAGE` 时仍先发SIP `200 OK`，再完成短信解析和耐久写入。RP-ACK报告只有durable callback成功后才生成并发送。Rust不接收原始SIP事件，也不拥有SIP 200、入库准入、当前注册socket/TCP stream、写请求队列、响应读取/派发、live report response transaction或IMS session teardown。出站报告的帧/CSeq由Rust构造和分配；Go按一次写入并等待响应的旧运行方式继续负责transport/event生命周期。

这是一段生产VoWiFi短信签名路径的真实所有权切片，不是完整IMS session/event owner迁移，不代表短信在线投递实测成功。无Rust coordinator 时Go按fail-closed处理，不再用Go生成report frame。9eSIM `6982`、reader/N待 inbound event owner、IMEI、运营商流量及硬件操作均未涉及。

## 验证

- `scripts/test-migration-gates.sh` 离线完整门禁：通过。Rust workspace 1,398 passed、6 ignored；其中 `switchd-rs` 903 passed、3 ignored。
- Go 全仓 `go vet ./...` 与 `go test ./...`：通过；定向 coordinator adapter 与 IMS RP-ACK path tests也通过。
- parity：运营商模板2,722项；USB status、module radio、public HTTP/HEAD、PCMA fixture、SDP及AMR framing通过。
- 迁移契约 `--diff`/普通扫描与43项工具自测通过；保持285条method/path（Rust213、Go72）、127个Swift HTTP调用点、251个审阅范围。Rustfmt及 `git diff --check` 通过。
- 当前分支隔离worktree路径较长，测试脚本临时将Go toolchain设为本机已安装的`go1.26.7`，为 Unix socket 用短TMPDIR，为不同既有parity fixture分别使用它们要求的 Switch/build 目录。只调整一次性测试环境，没有改门禁脚本。全程无网络下载、无读卡器/SIM操作、无运营商请求、无App构建或DMG。

## 进度

P3增加 **+0.025计划人周**至 **1.490/8.0（18.63%）**。整体累计 **13.395/47.5计划人周，约28.2%**（估算误差约±3个百分点）。Rust-only退出门禁仍未满足：IMS socket/session及事件生命周期仍在Go，Rust-only路由门禁仍因Go fallback及145条Go owner/dependency path失败。下一片应评估把SIP inbound event/response dispatch正式交给Rust，或者优先迁移reader IMS registration session owner；切分时必须保留已注册SWu/IPsec数据面与老wire行为。
