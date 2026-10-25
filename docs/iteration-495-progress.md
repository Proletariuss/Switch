# Iteration 495：注册刷新、N待会话组合与3.34阶段发行

2026-10-14。本轮实现原生同连接 REGISTER 刷新、N待按需卡片队列及双 Profile 会话组合，并补齐启动取消和安全诊断。Rust 全量 2692 passed / 0 failed / 6 ignored（daemon 1962/0/3）；Go vet/test、Swift BLE 38组、12组跨语言差分及45项扫描器自测通过。公共N待仍由既有Go运行时管理，普通原生接管开关仍关闭；未增加全Rust完成率。

## 本轮变更

- 原生 IMS 的唯一 SIP reader/writer 在已注册连接上发送 REGISTER 刷新；保持 Call-ID、Contact、目标和 Security-Verify，递增 CSeq 并更换分支。只有匹配响应和有效授予租期才能重新计时。401/407 的 AKA 异步执行，实际注册到期仍为硬边界；停止等待物理卡片任务和网络清理完成。
- 新增 Rust N待私有父归属和短事务能力，Go退出不清除该归属。多个 Profile 的准备/鉴权通过有界FIFO串行触卡，卡片事务释放后网络会话继续在线；单 Profile 停止不终止其他 Profile。最终退出恢复原 Profile，并在真实完成后释放父归属。HTTP不能构造这些私有能力。
- N待类型化运行时复用受保护 IMS 的收发短信和报告路径，始终SMS-only；普通通话仍为EVS优先、AMR-WB兼容。
- 原生启动阶段的Surge CLI取消会实际kill并wait；保留当前代理和DNS路径。状态提供静态、安全的阶段/错误码/中文说明，不输出私有SIP或鉴权材料，不改变原终止分类。

## 验证

Rust workspace：2692 passed / 0 failed / 6 ignored；daemon 1962/0/3。Go vet/test、Swift BLE物理完成证明和38组回归、12组跨语言差分及45项扫描器自测通过。

新增本机网络组合验证双Profile的IKE/受保护IMS、收发短信/RP-ACK、代次隔离、独立停止和原卡恢复；活跃通话覆盖刷新200/401/407、第二次定时刷新、AKA期间OPTIONS和停止、403/新安全协商/到期清理。对端和卡片为测试设施，不能替代真卡与运营商验收。

初次全量回归出现一项旧状态fixture未设置voice policy；改为调用真实consumers_ready，并增加SMS-only心跳恢复禁止语音断言后，重跑全量通过。N待网络fixture补齐ISIM文件路径和既有peer要求的状态报告标志；未放宽协议断言。原失败日志保留。

契约341（Rust308/Go33）、447审阅范围、127 Swift调用点、51 HEAD别名。Go退出仍6类阻塞/251生产源文件。未以新增测试、接口或类型化会话增加完成率。

## 保留边界

公共N待调度尚未切换到新类型化运行时，普通原生默认接管开关仍关闭。任何新的Security-Server协商目前都清理重建，会结束当前原生通话，未实现SA热替换。读卡器丢失/代次变化而无法确认恢复时，保留父归属并报告清理未确认；安全退役需后续补齐物理关闭证明。全Rust目标继续进行，不能标记100%。

## 安装包

按用户要求交付3.34/build334阶段DMG；Release构建、签名、镜像校验及只读挂载一致性核验通过。本机ad-hoc签名、未公证，未进行实卡或运营商测试。

[Switch 3.34 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.34-macOS27.dmg)，构建334，Apple Silicon，macOS 27.0及以上。

SHA-256：`10c65c34be1d4b11651e1a48e0342196ee571ad6b707ebb5590b385edc7e0ca3`。

证明与日志：`../build/reports/release-3.34/`。Release开始后只有两个测试文件与契约元数据调整，生产编译输入未变；最终全量回归已覆盖这些fixture。未启动或替换已安装应用，未拨号、发短信或操作真实卡片。
