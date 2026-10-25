# Iteration 481：读卡器原生生命周期、身份快照与租约

Rust 接管读卡器连接、断开、刷新、重建与确认掉线后的状态发布，并直接提供 PC/SC、BLE 会话租约。Go 不再持有这条链路的 metadata cache、pin/watchdog 或物理生命周期适配器。现有 IMS、N待及部分 eSIM 业务消费者仍由 Go 执行，因此本轮不是完整读卡器 Go-free，也不代表全 Rust 后端完成。

## 原生所有权与生产接线

- `reader_lifecycle::Service` 与同一 `Gate` 原子发布 reader、generation、完整 USIM/ISIM、eUICC 事实和物理 session。信息页直接读取原生快照；私有快照保留 SMSC PSI/target，公开响应保持原字段与隐私边界。普通刷新失败保留上一份有效身份。
- `NativeReaderSessionLease` 直接使用原生 supplier、PC/SC owner 与 Swift BLE 系统桥。旧 Go pin/unpin/metadata/finalize RPC 已退出；固定租约中的 APDU 排队、复位、切卡收尾和实际释放由 Rust 持有。
- 原生 physical producer 独立于受监督的 Go producer；Go 子进程重启不撤销有效的原生 reader selection、身份和租约。Go 仅消费受 generation/能力约束的不可变快照与 LPA 事务。
- 剩余 Go `reader/business-runtime` 只提供占用事实，并停止、等待被捕获的通信任务退出。停止、退役和恢复观察始终绑定原 Go producer，旧请求不能重新绑定或停止替换进程中的新会话。
- 公开 connect/disconnect/refresh/auth/info 保留既有 JSON、错误码与 HTTP 方法语义。连接请求保留原 Go decoder 的大小写字段、重复字段、null 和首个 JSON 值行为；405/Allow/HEAD 由 Rust 直接处理。

## 并发、失败和退出

切卡准备使用全局单一待准入槽，在停止旧通信会话前冻结 reader/generation。准备任务取消后继续等待真实停止；未知结果只观察，不重放 stop。若准备任务异常退出，原槽仍保留，直到捕获的业务终止或旧 Go 进程确实退出。N待与其他变更占用不会被抢占。

Service 的普通任务有界，并为已取得能力的收尾预留容量。已有事务的必要清理在 shutdown 阶段仍可完成，新连接、刷新和准备被拒绝。退出同时等待真实 job 与待准入槽，避免任务异常与恢复接管之间的计数空档被当作完成。

连接失败先确认旧/新物理连接关闭，再提交断开代次；不复用已关闭的旧身份，不重放未知 open/APDU。读卡器确实消失后，由原生生命周期处理精确 session 的关闭证明和代次退役。最终释放未确认时继续占用，不能由 HTTP 超时或取消提前放行。

重建使用不可变 operation receipt，重复请求不能再次复位。保留既有 500ms 收尾等待、ESTK 专用复位策略、目标 ICCID 核对、切卡警告及通知处理。通知复用真实 `list_with_cleanup`：交付成功并取得卡片确认后才移除；仍待发送的 enable/disable 条目返回警告，不能静默吞掉。

## 验证结果

完整迁移门禁通过：Rust workspace **2,267 passed / 0 failed / 6 ignored**，其中 daemon **1,631 passed / 0 failed / 3 ignored**。忽略项中的跨语言差分由门禁另行提供 fixture 执行，不重复计入上述 workspace 数字。Go build、全仓 test/vet、reader 定向 race、Swift BLE 完成证明及 **38 组**事件回归、**2,726** 项 carrier replay，以及 USB/radio/HTTP method/HEAD/IMS/SDP/AMR/MESSAGE/USSD 差分通过。契约工具 **45** 项与最终普通/差分契约检查通过。

Rust daemon 的非测试模式 `cargo build --locked -p proletariusd` 也通过（dev profile）；本轮未构建 Release App 或安装包。编译仍有未使用项和链接器警告，未将编译成功表述为零警告。

全量回归修正了五个仍模拟旧 Go pin/unpin 的恢复 fixture，改用直接原生 supplier 和实际阻塞的 fake PC/SC EndTransaction。保留取消、shutdown、未确认释放及精确租约恢复断言，且物理 transaction/EndTransaction 各只执行一次。测试没有被删除或跳过。

HTTP 差分 oracle 补入迁移前快照 `main.go:937–941` 中的五个真实注册。旧 fixture 漏列这些路径导致集合检查失败，修复保留了逐请求 status、Allow、HEAD 和 body 字节比较；没有放宽差分门槛。新增 BLE APDU fixture 核对仍待发送的 enable/disable 通知会警告且不会发出 REMOVE。

证据位于 Switch 根目录：

- `build/reports/iteration-481-migration-gates.log`：最终完整门禁。
- `build/reports/iteration-481-rust-production-build.log`：非测试模式 daemon 编译。
- `build/reports/iteration-481-native-recovery-tests.log`：53 项原生恢复测试。
- `build/reports/iteration-481-reader-lifecycle-tests.log`、`iteration-481-reader-control-tests.log`、`iteration-481-lifecycle-notifications.log`：生命周期、实际路由与通知边界。
- `build/reports/iteration-481-migration-gates-before-auth-method-expectation.log`、`iteration-481-migration-gates-before-reader-mux-fixture.log`：保留的中间失败及修复依据。
- `build/iteration481/`：Go build/test/vet/race、契约检查、工具测试与退出审计。
- `build/reports/iteration-481-baseline/source.tar.gz`：本轮修改前的不可变源码快照。

## 剩余范围、计分与发行

契约盘点为 **340** 个 method/path：Rust **304**、Go **36**；另有 **51** 个 HEAD alias、**127** 个 Swift 调用点、**387** 个审阅范围。仍涉及 Go owner/dependency 的路由 **143** 个；Go 结构退出仍有 **6 类阻塞**、**255 个生产 Go 文件**。这些计数只用于审计，不能换算成能力完成百分比。

前次已审计加权估值仍为 **18.195/47.5 = 38.31%**。本轮新增的是完整生命周期和租约供应职责；此前 USIM/ISIM APDU、PC/SC owner 和 generation snapshot 已有计分，不能再次累计。剩余包尚无这组职责的独立权重，本轮先记录接管证据，后续统一复核权重；不宣称迁移停滞，也不以任意增量制造精确进度。

下一完整工作包是普通单 Profile 的生产 IMS/VoWiFi 会话闭环：冻结完整 effective config；建立共享的注册会话事件分发与发送能力；接入短信、语音/USSD、续期、失败与退出；最后移除普通 reader 的 Go 入口。现有 `spawn_sms_session` 不能单独替换普通读卡器 VoWiFi，因为后者还提供语音。必须保留代理路径、自定义 ePDG/端口、EVS/AMR-WB、持久化后确认及未知发送不重放语义。

本轮没有实机、运营商、短信发送或通话验收，9eSIM `6982` 继续排除。未生成新 DMG、未改版本；已交付 **3.20/build 320** 的安装包只包含 Iteration 480，不包含本轮源码。
