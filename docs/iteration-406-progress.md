# Iteration 406：USSD 入站请求准入迁入 Rust

USSD dialog 中 INFO/BYE 入站请求的准入策略现由 Rust 根据对话建立状态、Call-ID tag、方法、CSeq 防重放和 INFO 包类型进行裁决。生产读卡器会话通过现有受认证 IMS codec 路由消费该策略；Go 继续负责 SIP 响应写入、XML 解码、channel 投递及 session/dialog 生命周期。缺少或非法策略响应会失败关闭。

Rust IMS/daemon 策略测试、Go bridge/imscore 定向测试及 race 测试通过；全量 `scripts/test-migration-gates.sh` 通过，日志为 [iteration-406-migration-gates.log](../build/optimization/iteration-406-migration-gates.log)，SHA-256 `9b36185ee9e320d996cfe734d1690708109bb17ae29eecb9888e5865b1a89e5f`。DMG 3.07/build307 包含本迭代；发行映像、归档哈希、bundle 版本/构建号及签名均已核验。

P3 +0.025，累计 2.640 / 8.0；全项目累计 17.345 / 47.5 = 36.5158%（显示 36.52%）。没有运营商 USSD 实机验收；本迭代未执行实体卡操作、拨号或短信测试。
