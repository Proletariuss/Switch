# Iteration 213：B1修复固化与Rust IMS响应分派

日期：2026-10-09（Asia/Shanghai）。全部源码与测试产物位于Switch内。

## 已保存的9eSIM修复

- `app/Sources/BluetoothReaderManager.swift`：重复连接不重置超时；等待记忆设备的新广播后连接新外设对象；保留NBLE认证与管理模式门槛，诊断仅记录字节数/计数器。
- 同文件的`displayName(forReader:)`通过本次设备元数据映射到`9eSIM_B1`。UI使用展示名，后台连接、选择与协议密钥派生仍使用原始标识和设备名。
- 展示已接入`ReaderCardView.swift`、`ActivityView.swift`、`MultiStandbyView.swift`，含蓝牙列表。
- 前一交付的2.94/294 App构建、33组BLE回归、DMG完整性与包内应用签名已经核验。该DMG早于本轮Rust组件改动，不代表包含本轮Rust新增代码。
- 本会话已核对14:26实机日志：9eSIM824699建立GATT，NBLE认证和管理模式检查成功，APDU往返及保活完成。EID/Profile页面、切卡、下载验收仍需独立证据。

## Rust推进

新增`sip_response_registry.rs`并接入`sip_session_runtime.rs`，只负责MESSAGE/OPTIONS请求的有界响应分派，不处理INVITE或注册事务。最多64项、最长60秒，键由Call-ID、CSeq、首个Via的sent-by/branch组成，并核对主连接。拒绝重复待处理键、歧义头、非法CSeq及容量超限；临时响应不释放槽位；终态、超时和取消释放等待。

`send_tracked_request`在写入前预约，`next_event`驱动截止时间并给匹配响应附带会话内票据。写入中途被取消时保留有截止时间的预约，写入错误/超时不会自动重发；远端结果可能未知，必须由短信事务层核对和持久化。新尝试须使用新的SIP事务身份。票据仅在创建它的runtime有效，不跨session代次使用。

该封装仍未接生产keyed actor；Go当前的IMS事件循环继续持有业务。此次没有开启native readiness，没有运营商网络、实卡写入或真实短信测试。

## 验证

- `cargo test --offline --manifest-path rust/Cargo.toml -p proletarius-vowifi`：161单测、2载波集成测试通过。
- 新增8个注册表测试，涵盖乱序、临时/最终/重复响应、错误连接/方法/序号/分支/Call-ID、超时和迟到、容量、取消、歧义头。
- 新增双端内存TCP网络集成测试：半帧到达后超时、恢复读取迟到响应、下一条请求准确完成、未自动重发及关闭资源。
- `cargo check --offline --manifest-path rust/Cargo.toml -p proletariusd`通过；16条既有dead-code warning。
- 严格Clippy仍被7项既有lint阻塞（5项载波配置嵌套条件、2项注册函数参数数目）；本轮新增代码的lint已修正，没有屏蔽警告，也不把该检查记为通过。
- 验证日志：`../build/optimization/iteration-213-vowifi-tests.log`、`iteration-213-daemon-check.log`、`iteration-213-vowifi-clippy.log`。

完整迁移检查首轮有1项伪终端测试失败（其余daemon 1115项通过、3项忽略）；独立复跑通过。检查发现模拟端写完响应即销毁PTY master，在并发负载下可能丢失未读数据。已仅调整该测试保留master到discovery完成，未修改生产modem逻辑；完整复验通过Rust workspace测试/格式、Go全仓test/vet、载波及USB/radio/HTTP method/HEAD/codec/SDP/AMR协议兼容性检查。完整复验日志为`../build/optimization/iteration-213-migration-gates-recheck.log`。

## 进度和下一步

全Rust加权进度保持31.4%（误差约±3个百分点）；新增组件没有冒充生产迁移完成。下一步接入keyed actor的会话代次/停止门槛，并让入站短信durable commit之后再产生RP-ACK。语音政策已同步为EVS优先、AMR-WB次选，禁用AMR-NB和PCMA。本轮未生成DMG。
