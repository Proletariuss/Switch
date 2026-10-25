# Iteration 494：会话能力统一、状态核对与3.33阶段发行

2026-10-14。本轮统一原生会话卡片能力接口，保留普通读卡器长期租约、AKA、心跳及确认释放行为；修复原生会话列表、停止/退避期间的就绪状态和 GET/HEAD 展示。Rust 全量 2667 passed / 0 failed / 6 ignored（daemon 1937/0/3）；Go vet/test、Swift BLE 38组、12组跨语言差分及45项扫描器自测通过。普通原生接管开关保持关闭；N待按需卡片 provider 和活跃通话 REGISTER refresh 尚未完成，未增加全Rust完成率。

## 本轮变更

- SessionCard 统一会话依赖，普通会话仍委托原有读卡器长期租约。ISIM/USIM 选择、冻结订阅身份、心跳和确认释放顺序不变。新增按需能力接口为后续N待共享卡片工作流预留，尚无生产provider，不将其计为N待网络会话接管。
- 原生会话存在时，公开 sessions 接口合并既有Go记录与当前原生记录，保留Go行和扩展字段，按读卡器、代次和SIM精确去重。旧列表读取失败显式报告不可用；没有可用原生记录时返回503，避免把未知结果当空列表。
- 停止、清理和退避不再复用历史监听器就绪事实，显示独立 stopping/cleanup_confirmed；保留关停期间状态可读。读卡器 VoWiFi 状态和 sessions 的HEAD规范化到对应GET处理。
- EVS优先/AMR-WB兼容、代理语义和默认原生接管开关未变。

## 验证与边界

Rust workspace：2667 passed / 0 failed / 6 ignored；daemon 1937/0/3。忽略的差分项由独立12组跨语言验证执行。Go vet和全包测试、Swift BLE操作完成证明与38组管理器回归、45项扫描器自测通过。

新增真实本机HTTP→Service→模拟PC/SC→本机IKE/受保护IMS网络对端组合验证列表、HEAD、信令EOF、退避与停止；原有5个启动/短信/通话/续期组合继续通过。此处网络及卡片对端为测试设施，不代表真实运营商或真卡验收。首次测试暴露macOS socketpair在完整响应后half-close返回ENOTCONN，修正仅测试收尾断言后复验通过；原始日志保留。独立SessionCard审阅未发现普通租约等价性阻塞。

契约341（Rust308/Go33）、439审阅范围、127 Swift调用点、51 HEAD别名。Go退出仍6类阻塞/251生产源文件，未移除本轮Go生产文件。不按测试数量增加完成率。

活跃原生通话仍可能延后REGISTER续期超过授予Expires；本轮仅审阅方案，未落地无损刷新。Go现有路径到期重建也不能代表已有无损续期。全Rust目标继续进行，不能标记100%。

## 安装包

按用户要求交付3.33/build333阶段DMG；Release、签名、镜像校验及只读挂载一致性核验通过。本机ad-hoc签名、未公证，未进行实卡或运营商测试。

[Switch 3.33 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.33-macOS27.dmg)，构建333，Apple Silicon，macOS 27.0及以上。

SHA-256：`6bea75c655a0ea41e7f38569d2e1432135b9452ffd49056ba725d0a8b35427f3`。

验证证明、源码冻结哈希及完整日志：`../build/reports/release-3.33/`。本轮未启动或替换已安装应用，未拨号、发短信或操作真实卡片。
