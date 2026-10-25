# Iteration 181：读卡器号码回退直接读取 Rust 备注存储

日期：2026-10-09。所有源码与验证产物仅位于 `/Users/proletarius/Desktop/Switch/`。

读卡器信息 Rust 投影现在直接从 Rust profile-notes store 查询缺失号码的 Profile 回退；Go 私有 `/api/internal/reader/info-facts` 适配器只提供物理读卡器/SIM事实及活动 eSIM 显示名，不再回调 Rust RPC 获取同一号码数据。有效 SIM 号码优先；缺失号码识别、ICCID 归一化、无备注时省略可选号码字段保持原语义。存储错误仍作为空号码处理，和原 API 表现一致。

定向 Rust 投影测试、Go reader-info adapter 测试、Rust 格式检查及契约差分通过。完整 `bash scripts/test-migration-gates.sh` 通过：Rust daemon 1,018 项通过、3 项忽略；Go 全仓测试与 `vet` 通过；2,722 项载波回放以及 USB/radio/HTTP-HEAD/codec-SDP parity 通过。当前契约为 313 条（Rust 247、Go 66，含 43 个 HEAD alias）、127 个 Swift HTTP 调用点、290 个审阅范围。

未进行读卡器/号卡实机、VoWiFi、Go-free构建/运行或App/DMG验收。此次只从读卡器信息链路去掉冗余跨进程号码查询，公开路由仍依赖Go提供物理事实。保守计入P2B +0.025计划人周；基于Iteration 180的14.245/47.5累计值，本轮后为**14.270/47.5（30.0%）**，P2B为**5.100/8.0（63.8%）**，误差约 ±3 个百分点。