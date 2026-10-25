# Iteration 080：扩展 Rust modem typed-query 覆盖

日期：2026-10-05。所有变更与生成物只写入 `/Users/proletarius/Desktop/Switch/src`；Celmux 参考副本未修改。

## 实现

在上一轮认证的 `POST /api/internal/modem/typed-query` 白名单中，新增 8 种只读查询：注册状态、LTE serving cell、APN、IMS 注册状态、网络制式/双工/频段/信道、SMSC、本机号码、USBNET 模式。与 Iteration 079 的身份、SIM 状态及 CSQ 查询合计 14 种固定查询。Rust actor 映射固定 AT 命令并解析字段；没有新开任意 AT 通道，也没有增加公共 Swift API 或修改持久化格式。

Go `Manager` 的既有方法现在通过 Rust 生产串口 owner 取得这些结果，并保持既有 API 返回结构、注册状态本地化、飞行模式探测抑制和旧串口 fallback。服务小区无效响应仍返回零值且不当成传输错误；USBNET 格式无效仍返回 `-1,nil`；CNUM 继续跳过标签/无效号码并规范化第一个有效号码。查询仍通过单一 bounded Rust actor 调度串口。

## 验证

- `cargo +1.98 test --manifest-path rust/Cargo.toml --offline -p proletariusd`：**351 项通过**。伪串口验证固定 CSQ、CREG 命令与结果；Rust fixtures 覆盖 CREG、QENG、CGDCONT、QIMS、QNWINFO、CSCA、CNUM 与 QCFG 解析及 malformed 行行为。
- `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local go test ./internal/modem -count=1`：通过。验证 typed result 投影、注册状态文本及 malformed 值、服务小区/网络/SMSC/CNUM/USBNET 返回字段、空身份值和蜂窝飞行模式抑制。
- `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local go test ./cmd/proletarius-macos -count=1`：全包通过，Unix socket fixture 验证认证、typed-query 注册/网络字段 JSON 以及不接受任意 AT 字符串。
- Rustfmt、Go fmt、`git diff --check` 通过。API/UDS census 仍为 196 项：Rust 93、Go 103；typed-query 是既有内部路径的白名单扩展，没有新增 method/path。保留两个与本轮无关的 `native_vowifi_session.rs` dead-code warning。

没有连接实体 modem/读卡器、访问短信、改动 SIM、调用运营商，也没有生成 DMG。operator `COPS` 格式设置、fallback `COPS?`、APDU/SIM metadata、URC 业务分派与 modem 高层状态聚合仍在 Go；本轮不代表 modem 业务已全 Rust 接管。

## 进度

固定只读 modem 查询现覆盖 Go Manager 日常采集与状态方法的一组 14 项命令，且有 Rust 生产 owner 接线和离线兼容回归。按权重将 P2B 从 30% 调整为 33%（8.0 人周工作包增加 0.24 人周等值）；全项目从 **21.4%（10.16/47.5）** 调整为 **21.9%（10.40/47.5）**，估算误差仍约 ±3 个百分点。该比例不表示实体设备验收、全 Rust 后端完成或可发布。
