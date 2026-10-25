> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# 迭代 045：IMS 身份在 session preparation 阶段绑定

- 版本：2.24 / build 224（2026-09-25）
- 基线：2.23 已把 ISIM IMPI/IMPU 安全地送达 Rust lease，但 native preparation 仍未消费身份；选择 `isim` 的缺失配置可能要等 REGISTER 编译才发现。
- 实施：抽出共享的 snapshot identity resolver。`NativeSessionContext::prepare` 在 transport/proxy 选择前，以同一 reader/SIM generation 的 lease identity 解析并持有 zeroizing IMPI/IMPU；REGISTER profile 编译继续调用同一解析逻辑。`isim` 缺身份、`auto` 部分身份和无效 IMSI/ISIM 均保留明确 fail-closed 语义。身份不加入 response、日志或 Debug。
- 验证：VoWiFi 132 项、daemon 161 项测试，Rust 格式检查和 `cargo +1.98.0 build --release -p proletariusd` 通过；新增 synthetic ISIM lease→resolved profile 回归和不完整身份错误码断言。
- 收益/成本判断：将本可在 REGISTER 阶段才暴露的配置错误提前到代理与 IKE 之前，避免无效 IMS 身份造成无意义网络启动；只增加共享 resolver 和敏感值绑定，无新依赖或 API，符合收益大于维护成本。
- 边界：计划诊断请求结束时仍会释放 reader lease；没有启动生产 Rust tunnel/IMS owner，没有真卡、ePDG、SMS/呼叫验收。生产 ownership 保持 9/15（60%），Go 实时 owner 未删除。
- 不生成 DMG；此轮持续 Rust 迭代，未收到新的单独构建要求。
