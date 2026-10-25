# Iteration 212：9eSIM B1 NBLE 与 LPA 端到端核对

日期：2026-10-09。源码与构建产物只写入 `/Users/proletarius/Desktop/Switch/`。`/Users/proletarius/Desktop/Proletarius-iOS` 和 Clarivate_IOS 对话只读。

## 结论

Switch 已实现 B1 的独立 NBLE 认证与 APDU 传输，并把它接入 Rust 的 BLE APDU 客户端和 eSIM/LPA profile 读取、切换、下载业务路径。与 Proletarius-iOS Dart 实现逐字段核对，服务/特征 UUID、默认密码和密钥派生、DES-EDE challenge-response、分片/校验、counter、迟到响应处理、管理模式探测及 keepalive 均一致。没有发现需要从签名版 APK 提取的 NBLE 密钥或专属签名。

`NekokoLPA2 v2.2.6+588` 所标注的 `9eSIM Signature` 是 Android APK 签名身份；发布页只提供证书指纹和已签名 APK，不提供也无法从 APK 还原签名私钥。该 APK 证书不用于 Mac CoreBluetooth NBLE challenge 或 Switch 的 macOS 代码签名。Switch 复用了可审查的协议实现线索，而不是 APK 二进制或其私钥。

## 来源历史

- 只读检查 `/Users/proletarius/Desktop/Proletarius-iOS/lib/adapter/ble/nine_esim_protocol.dart`、`nine_esim_adapter.dart` 与 `IOS_VERIFICATION.md`。
- 新收到的 `/Users/proletarius/Downloads/Clarivate-9eSIM-NBLE-2.2.11-597-20261009.zip` 是从该工作区摘取的代码与测试，不是上游安装包；ZIP 的 25 个清单文件 SHA-256 全部校验通过，NBLE protocol 文件哈希与只读项目当前文件一致。归档内保留 MIT LICENSE；Switch 第三方通知文件已有对应版权及许可条款。
- 归档中的参考适配器允许在构造时指定非默认密码，但 README 明确没有配置 UI；Switch 目前仅使用设备标识派生出的默认密码，未引入自定义密码界面。当前材料未显示目标 B1 使用自定义密码；若实际设置过自定义密码，还需提供受保护的密码配置入口。
- 只读查看 Codex `Clarivate_IOS` 对话。用户确认参考项目 B1 连接、EID/Profile 读取及切换通过；后续测试日志分析显示一份 8,032 字节 profile 安装完成，两个卡内通知均由服务器 HTTP 204 确认并从卡片移除。
- `IOS_VERIFICATION.md` build 597 的历史状态没有追记上述较晚实测。因此将聊天实测与旧验证文档分开记录；两者都不能作为 Switch 本机实测证据。

## Switch 验收

- NBLE 生产代码：`app/Sources/BluetoothReaderManager.swift`；APDU 系统桥：`app/Sources/ReaderBLEBridge.swift`；Rust APDU 边界：`rust/crates/proletariusd/src/reader_ble_apdu.rs`；LPA handler：`rust/crates/proletariusd/src/reader_esim_download.rs`。
- `BLE_MANAGER_ITERATIONS=100 sh scripts/test-ble-manager.sh`：30 组生产事件/协议测试各重复 100 次，通过；覆盖 NBLE 向量、认证门槛、eUICC 管理模式、checksum/counter 与迟到响应。
- 新增协议对照回归：CRC-24/OPENPGP 密码正规化、两条归档 APDU framing 向量和长 APDU 5-bit 序号回绕；纳入上述 100 轮测试。
- `sh scripts/test-reader-ble-selection.sh`：13 项设备选择回归通过。
- `cargo +1.98 test --offline --manifest-path rust/Cargo.toml -p proletariusd reader_esim_download::tests`：45 项通过。包含 BLE APDU reader 绑定、profile 切换 LPA、profile 读取/改名/删除，以及下载桥错误语义。下载成功事务本身仍需真实卡和有效测试 profile 验收。
- 本机当前系统蓝牙清单没有已连接的 9eSIM B1；因此没有宣称 Switch 已真机读卡、切卡或下载成功。没有使用用户历史中触发 `6982` 的测试码，也没有执行写卡操作。

## 发布与剩余验收

本轮将应用版本更新为 2.94（build 294），生成 Release arm64 DMG 后需核验签名、镜像完整性和 SHA-256。当前全 Rust 后端目标仍继续推进；此应用发布不代表 Go 退出，也不将 9eSIM 专有 SM-DP+ `6982` 纳入修复范围。

尚需用 Switch 在实机验证：B1 认证连接、EID/Profile 列表、切换并恢复原 profile、用新的有效测试码完成一份非 `6982` profile 下载及通知清理、断线重连与空闲 keepalive。
