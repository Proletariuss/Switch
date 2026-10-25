# Iteration 501：USB eSIM 原生观察、旧 Go IMS 退役与 3.40 发行

2026-10-14。用户明确要求生成 DMG，本轮收齐在途改动后验证发行，不将全 Rust 目标标为完成。

## 实现与保护

USB eSIM overview/refresh/health 与 rename/delete 后刷新使用原生观察；缓存由 USB owner 持有，绑定 producer、物理 epoch、session 和 revision。普通读取使用有效缓存；明确刷新保留同卡最近成功快照用于既有只读回退；写入前清理缓存，旧读取无法在新代发布。健康检查持有同一射频保留，固定 AT 观察检查租约、原生持有、inhibit 与 cellular suppression 变化。取消/超时仅结束 HTTP 等待，实际任务持有射频与物理工作直到真实返回并释放。

读卡器 Mutation 不再无条件请求已退役的 Go 停机接口：使用精确维护能力、reader/generation、StopAndDrain 与 ordinary_drained 证明；原生 worker/物理清理仍由保留任务负责。真正捕获的旧 producer 仅在实际退出换代后退休，未知旧 claim 返回未确认，不假定已停止。

删除28个旧Go生产文件，抽取1个仍用于SIM/SMS/BLE的共享受限RPC文件，净减少27个；不将旧会话整体搬到测试文件。纯协议/身份差分oracle保留。仍活跃的Go观察守卫读取Rust真实RF owner，包括准备阶段radio_owned；错误保守拒绝。保留modem实际停止与PCSC物理关闭，启动bootstrap不依赖该观察成功，不形成初始化自锁。

## 验证

完整门禁通过：Rust workspace 2,763 passed / 0 failed / 7 ignored，其中4个差分夹具在后段显式通过，剩余3个互操作夹具未在此次门禁执行；Go vet/test、Swift BLE、13组差分和49项扫描器自测通过。定向USB87、overview5、health3、eSIM操作14及读卡器维护/退出90项通过。取消、inhibit、物理换代与缓存失效采用实际本地工作线程/脚本化USB传输验证，没有真实号卡操作。

新增测试早期缺少完整eUICC探测脚本和与main一致的operation journal初始化，修复测试准备后保持原断言并通过；失败日志保留。独立审阅补齐健康inhibit/抑制状态检查，并修订7条保留HTTP接口的历史Go消费者说明。发行验证还核验真实只读挂载来源、动态依赖实际解析和Mach-O最低系统版本。

## 发行及剩余边界

[Switch 3.40 / build340](release-3.40.md)通过正式构建、签名、DMG校验及挂载逐文件比较。最终源码冻结后至验证结束零漂移；发行后仅更新本轮文档、README和目标进度。

315条method/path契约=Rust309+Go6；451个审阅scope、127个Swift HTTP调用、54个HEAD别名。相对501基线只删除2条真实Go私有接口；89条仍有Go归属或依赖。退出扫描214个生产Go文件、6类阻塞，非USB eSIM、发现/配置、蜂窝兼容和协议库仍在。离线结果不等同实卡/运营商/长稳验收；不根据测试量或路由数虚增历史进度百分比。

证据：../build/reports/iteration-501-full-gates.log、iteration-501-contract-diff.log、iteration-501-go-exit.log、各职责独立审阅，以及release-3.40/verification.json和gate-summary.json。
