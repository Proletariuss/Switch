# Iteration 500：模块 VoWiFi 原生公开接管与 3.39 发行

2026-10-14。用户明确要求生成 DMG。本轮只完成当前改动收口和发行，没有将全 Rust 目标标为完成。

## 接管与生命周期

模块 GET/POST /api/vowifi、操作结果、共享 IMS 通话及 USSD、混合会话列表接入持有实际会话的 Rust Service。保留请求字段类型、大小写与重复字段顺序、取消/超时错误和原有方法/HEAD/Allow行为；退役16个 Go 公开 mux 注册与4个无消费者私有注册。模块原生状态绕过 Go 恢复等待，状态轮询只观察 CFUN，缓存投影不追加身份 APDU。

模块切卡、重命名、删除与下载先通过同一已注册 actor 原子检查通话状态，再停止并等待网络任务、物理工作及 RF 恢复；下载仍保留旧外部错误分类。正常 USB Close 无法撤销被原生会话持有的精确 USB 归属。仅实际端口关闭、在途工作完成并匹配原代次时，才允许以 restoration_skipped_reader_lost 明确退役，不能向替换 USB 发送旧 CFUN。

独立审阅发现 Go 退出清理结束与 producer 退休之间可能抢入新 start。现即使此前未初始化模块 Service，也先安装临时启用保护，再真实清理；新 Go 就绪后才解除保护，永久 shutdown 不被复活。真实组合验证清理期间/之后拒绝启用，恢复准入后才允许真实准备及取消。

## 验证与纠错

最终完整门禁通过：Rust workspace 2,771 passed / 0 failed / 7 ignored；其中4个差分夹具在门禁后段显式执行，其余3个互操作夹具未在本次门禁运行。Go vet/test、Swift BLE39组、13组协议差分、49项扫描器自测通过。

真实本机模拟组合经公开 HTTP 驱动 USB、CFUN、AKA、IKE、受保护 IMS 注册、双向短信精确 Accepted 与模块归档、EVS RTP/PCM和通话历史、取消、切卡与 Go 退出；USB关闭证明3/3、原子切卡6/6、模块卡8/8通过。没有操作真实卡片、向真实号码发信或进行运营商/真人音频验收。

完整门禁先发现3处仍假设旧 Go 接管的夹具，更新为真实原生行为及剩余私有透传覆盖。另一个既有测试在 commit 前发出 started 信号，取消可合法提前返回，造成 !job.is_finished 偶发失败；仅 cfg(test) 增加实际线程完成屏障，保留并加强原断言，专项8/8和最终全量通过。全部失败日志保留，不以重跑隐藏问题。

## 发行与未完成边界

[Switch 3.39 / build 339](release-3.39.md) 的 Release、严格签名、DMG校验、只读挂载逐文件比较及独立发行检查通过。构建后仅两个契约元数据与上述 cfg(test) 文件变化，生产编译输入无漂移；发行后更新本文、README及目标记录。

契约317条=Rust309+Go8；127个Swift调用、465个审阅范围、54条HEAD别名。相对320条基线，删除4个私有接口，显式登记1条既有隐式HEAD别名；不把该别名计作新增功能。97条路由仍有Go归属/依赖，结构性退出检查仍有6类阻塞、241个生产Go文件。Go发现/配置、兼容观察和相关库仍保留，实机与长稳验收尚未完成；历史38.31%账本未核平，不生成新百分比。

最终证据：../build/reports/iteration-500-full-gates-final.log、iteration-500-contract-diff-fenced.log、iteration-500-module-transport-fence-final.log，以及 release-3.39/gate-summary.json、verification.json 和独立审阅报告。
