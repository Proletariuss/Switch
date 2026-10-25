# Iteration 499：续期生命周期修复与模块迁移基础

2026-10-14。用户要求生成 DMG，发行 3.38 / build 338。全 Rust 目标仍在推进，未完成模块公开接管或最终 Go 退出。

## 已上线的协议修复

对端发起 CHILD 换钥后，原控制状态可能一直保留旧 SA，阻塞后续换钥。现在等数据面宽限结束后，以准确的旧 SPI 发出 DELETE，丢包重传使用冻结报文，控制面 ACK 与数据面退休都确认后才释放旧状态；交叉 DELETE 不误删新会话。

AUTH_LIFETIME 独立记录绝对鉴权期限，IKE 换钥不会延长它；零值需要先完成真实响应写出，再清理并重新做 EAP/IMS 注册。生产组合暴露出网络任务结束时过早停止心跳，关闭的 lost watch 被误判为读卡器丢失，阻止计划重建。现将心跳保留到已注册 actor、consumer 和网络真实 join 后再停止。

交叉审查又发现真实丢卡和计划退出同时发生时，内部取消可能盖过丢卡原因。现从仍被持有的真实 watch 冻结 true/异常关闭事实，分类时先于计划重试处理，用户明确停止仍由外层优先处理。三种 watch 的真实任务 join 回归通过，未改写 actor/consumer 的退出原因来制造成功。

## 代码退役与未启用的模块基础

删除已无生产调用的 Go reader 启动、EAP 客户端、网络/IMS 包装及旧短信分支；保留模块网络、鉴权、SMS、call/USSD 和配置路径。Go 模块会话列表投影真实活动状态，reader 查询 busy 不再误把模块状态当 reader 身份占用。未把退役实现复制为测试实现。

新增冻结 USB producer/session/代次与 RF intent 的模块卡适配。只允许进程内 typed capability 在其自身 RF 保留期间执行身份/AKA；公共请求不能构造此权限。真实物理工作保留到完成，丢弃调用者不释放 RF，恢复失败保留归属。新增模块 IMS 短信绑定与来源归档，Go 子进程退出不误回收原生发送。

模块 Service 尚无生产初始化调用，GET/POST /api/vowifi 仍使用既有 Go 模块入口。新增模块分支是迁移基础，不能宣称模块已全原生接管。公开路由、主退出顺序、eSIM 停止钩子与完整模块协议组合仍需后续完成。

## 验证与交付

完整门禁通过；最后丢卡竞态修复后再次运行 Rust workspace：2,759 passed、0 failed、7 ignored。Go vet/test、Swift BLE、13 组协议差分、49 项扫描工具自测通过。契约 320 条（Rust owner 295 / Go owner 25），127 个 Swift 调用、462 个审阅范围；路由与归属没有变化。

模块卡 7/7、RF 85 通过/1 既有差分忽略、USB AT 80/80、设备短信 41/41、发送账本 10/10；真实本机协议组合通过双轮 CHILD 换钥、MO/MT 短信、AUTH_LIFETIME=0 的新鉴权注册、EVS 跨换钥媒体和终态历史。真实 watch/join 竞态也已验证。所有这些均为离线或本机模拟对端证据，没有操作真实号卡或运营商。

Release App 与 DMG 3.38 严格签名、镜像校验、只读挂载逐文件比较、独立发行审查通过。仅源码审阅元数据在构建期间更新，编译输入无漂移。[发行记录](release-3.38.md)。所有中间失败日志保留，最终证据分别见 iteration-499-full-gates-final.log、iteration-499-rust-workspace-post-review.log 与 release-3.38/verification.json。

结构性 Go 退出仍有 6 类阻塞，生产 Go 文件 241 个。历史 38.31% 账本尚未核平，不据删文件或测试数量更新百分比。继续完成模块真实公开接管、残余 Go 依赖和设备验收；未生成新的完成百分比，目标保持 active。
