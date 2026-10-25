# Iteration 493：N待卡片工作流接管与3.32阶段发行

2026-10-14。本轮将 N待 的身份读取、切卡恢复、通知检查和 USIM/ISIM AKA 生产流程迁入 Rust，删除 Go 短信缓存及四个内部删除/清理入口，并修复原生呼叫早期状态和来电号码投影。Rust 全量 2665 passed / 0 failed / 6 ignored（daemon 1935/0/3）；Go vet/test、Swift BLE 38组、12组跨语言差分、45项扫描器自测通过；契约341（Rust308/Go33）、437审阅范围。Go退出仍6类阻塞/251生产文件；普通原生接管开关保持关闭，未做实卡或运营商测试，不按测试数量增加完成率。

## 生产变更

- N待继续使用原有业务排队与短事务能力，Rust 完整执行 EID/AID 验证、实际 USIM 读取、ES10c ENABLE、复位/重建、通知同步、目标 ICCID/IMSI 验证及 USIM/ISIM AKA。Go 保留窄协议客户端和网络会话；旧卡片算法只保留测试专用对照。
- 一个不可从线协议伪造的 PC/SC guard 覆盖多段物理操作。取消、超时、租约退休及退出不提前宣告物理结束；已提交切卡仅允许一次不超过45秒的恢复，不授权新的 AKA。ENABLE 前清除旧身份，成功复核后发布新身份，保持读卡器代次不变。
- 同一 operation ID 查询原回执；响应丢失不重放 ENABLE。未知任务通过取消记录阻止迟到提交，完成回执只在原能力退休后回收；取消回执回收后再次出现 unknown 也可收敛。`worker_finished` 仅说明该工作线程及其在途操作已结束，Go 随后仍依次确认物理租约和业务租约释放，不将其等同于物理句柄已关闭。
- 短信清空、删除和按发送者删除已完全使用 Rust 归档/USB owner。删除七个 Go 生产文件及四个内部清理入口；公开短信存储 GET/HEAD 和 405/Allow 行为保留，接收短信仍走耐久归档确认。
- 原生180/183无SDP响应正确发布早期呼叫状态，同时不凭临时响应启动媒体；取消状态不被迟到响应复活。来电及通话历史展示提取后的号码，完整From头仍保留在SIP对话。

## 交叉审阅与验证

三路执行/交叉审阅覆盖 Rust 工作流、Go 客户端、PC/SC 保留与退休、短信归档、路由鉴权及关停顺序。冻结基线为4691文件，未重置已有工作区修改。

- Rust workspace：2665 passed / 0 failed / 6 ignored；daemon 1935/0/3。忽略的差分项由独立12组跨语言门禁执行。
- Go vet及全包测试、Swift BLE operation proof和38组manager回归、45项扫描器自测均通过。
- 新卡片专项13项包含真实 HTTP→Service→Gate→PC/SC worker→实际LPA/APDU编码与解码，验证切卡、重开、通知检查、ISIM AKA、重复任务不二次鉴权、IMSI变化拒绝及实际lease释放/close配对。卡响应为模拟输入，不能替代真卡验收。
- PC/SC模块43项含阻塞ENABLE、退休/过期/关停及close未结束不得确认release。Go客户端20项专项组覆盖丢响应、取消、unknown fence与私密SMSC字段。
- 契约341（Rust308/Go33）、127 Swift调用点、437审阅范围、51 HEAD别名。删4条Go私有路由、增3条Rust卡工作流路由。
- 初轮失败来自旧Go路由数量断言及差分legacy短信存储清单，修正并完整重跑；原日志保留。最终日志与发行证明在`../build/reports/release-3.32/`。

## 安装包与边界

按用户要求交付3.32/build332阶段DMG，Release、签名、镜像校验、只读挂载及包内一致性验证通过；本机ad-hoc签名、未公证。

[Switch 3.32 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.32-macOS27.dmg)，构建332，Apple Silicon，macOS 27.0及以上。

SHA-256：`0b0fd90c7117c7e98a93dc7818fe8ea69a33671eb13a7fd81f7283026272e516`。

未操作真实读卡器、未发短信或拨号；本轮未新增完整BLE卡工作流真机验收。普通VoWiFi默认原生gate仍关闭，Go N待网络运行时、模块/普通会话等依赖仍存在。历史38.31%保留为先前核算值，不作为当前完成率；本阶段包不代表全Rust目标完成。活跃通话续期和Go完全退出等验收仍须继续。
