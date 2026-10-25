# 9eSIM B1 NBLE 移植记录

日期：2026-10-09。改动仅位于 Switch 源码仓库；`Proletarius-iOS` 只作为只读参考。

## 已移植内容

`app/Sources/BluetoothReaderManager.swift` 增加独立的 9eSIM B1 NBLE 传输。它通过 Nordic UART 的 Service/Write/Notify UUID 识别 B1，并将该传输与旧 AE30 BeeSIM、ESTKme CCID 和 legacy 协议分开。

移植的连接流程包括设备名末尾六位标识及默认认证口令派生、两密钥 DES-EDE 挑战应答、认证 ACK 校验、`FE 0D 00 00 01` eUICC 管理模式读取，以及成功后才发布已连接状态。已实现 100 字节 APDU body 分片、帧序号与末帧标志、16 位加和校验、响应重组、响应计数核对和迟到响应丢弃。空闲连接按 12 秒节拍检查，在空闲超过 10 秒时发送 NBLE keepalive。

认证材料只在连接期间保存在内存中，诊断日志不记录密钥、挑战应答或 APDU。没有把 AE30 BeeSIM 复位指令套用到 NUS 通道；B1 的 reset 请求返回明确的“不支持”错误，避免发送未经验证的控制帧。eSIM Profile 读写继续走现有 Rust LPA/APDU 路径。Profile 启用时已有的外部管理模式 `refreshFlag=false` 保持不变。

## 逐项对照与验证结果

协议来源是用户提供的只读项目 `/Users/proletarius/Desktop/Proletarius-iOS/lib/adapter/ble/nine_esim_protocol.dart` 和 `nine_esim_adapter.dart`，以及打包摘录 `/Users/proletarius/Downloads/Clarivate-9eSIM-NBLE-2.2.11-597-20261009.zip`；它们与 Swift 实现按字段核对如下：

| 环节 | 参考实现 | Switch 实现 | 核对结果 |
| --- | --- | --- | --- |
| 设备/服务选择 | 设备名后六位标识；Nordic UART Service | 名称含 9eSIM 且发现 NUS 后选择 NBLE | 一致，名称不会单独触发旧 BeeSIM framing |
| 默认认证 | 六位标识倒序作为默认密码；末两位标识参与派生 | `defaultPassword`、`deriveKey` 同规则 | 一致 |
| 认证握手 | 16 字节 challenge 经两密钥 DES-EDE 解密，计数加一后加密回传 | CommonCrypto 3DES ECB，K1-K2-K1，调整同一计数字段 | 与 Dart 参考向量一致 |
| APDU 封装 | counter + APDU + 16 位加和校验；每片最多 100 字节；5-bit 序号/末片位 | `NineEsimProtocol.encode` 同规则，并将每片作为一个 GATT write | 一致 |
| 响应 | 重组后验证序号、checksum 与响应 counter；旧响应丢弃 | `NineEsimResponseDecoder` 同规则 | 一致 |
| 会话门槛与保活 | 认证 ACK、管理模式 APDU 成功后才开放；空闲 10 秒以上以 12 秒周期 keepalive | 同样的认证/管理模式门槛与空闲 keepalive | 一致 |
| LPA 操作 | NBLE 只承载 APDU；下载、profile 读取/启停走 LPA | CoreBluetooth → 私有 `ReaderBLEBridge` → Rust `BleApduTransport` → 同一 LPA 业务实现 | 设计边界一致；Switch 真卡结果需独立验收 |

### 关于发布包“9eSIM Signature”

[NekokoLPA2 v2.2.6+588 发布说明](https://github.com/iebb/NekokoLPA2/releases/tag/v2.2.6%2B588) 将若干 Android APK 标注为 `9eSIM Signature`，并公开 APK 签名证书的指纹。这是 Android 包签名/证书身份，不是 NBLE challenge 密钥，也不是 macOS `.app` 的签名身份。公开 APK 可用于核对证书指纹，不能从中恢复签名私钥；将 APK 签名移植到 Switch 不会改善蓝牙认证或 LPA 下载，因此 Switch 没有捆绑这些 APK 或伪造签名。Switch 的 NBLE challenge 使用从 B1 设备标识与认证口令派生的会话密钥。

Clarivate 摘录 ZIP 的 `SHA256SUMS.txt` 中 25 个文件均通过校验；其中 `nine_esim_protocol.dart` 的 SHA-256 与只读项目当前文件一致。该 ZIP 保留 MIT 许可证，Switch 的 `THIRD_PARTY_NOTICES.md` 已保留对应通知。摘录版参考适配器允许调用方传入可选自定义密码。2026-10-10 补充：Switch 蓝牙设备列表已提供钥匙图标密码入口，按 CoreBluetooth 设备 UUID 在本机钥匙串保存自定义密码；未设置时按 B1 默认规则派生，恢复默认只删除当前设备的自定义项。明确认证拒绝或钥匙串读取失败时停止自动重试，等待用户检查后手动连接。

## 验证结果与边界

- `scripts/test-ble-manager.sh`：30 组生产 BLE 状态机事件测试，每组重复 100 次，通过。
- `scripts/test-reader-ble-selection.sh`：13 项设备选择回归，通过。
- B1 合成向量覆盖默认口令/密钥派生、DES-EDE 挑战、APDU 分片、序号/末帧、校验和、迟到计数响应、认证 ACK、eUICC 模式门槛及 reset 防误发。
- Clarivate_IOS 对话记录：用户实测参考 iOS 项目 B1 连接、EID/Profile 读取和切卡通过；随后提供的成功日志显示 8,032 字节 profile 完成安装，两条运营商通知收到 HTTP 204 并从卡内移除。其随附 `IOS_VERIFICATION.md` build 597 仍写切卡待复测且未宣称下载，记录晚于该对话实测。此证据只证明参考 iOS 项目，不能替代 Switch 真机验收。
- Switch 离线验证：`BLE_MANAGER_ITERATIONS=100 sh scripts/test-ble-manager.sh` 的 30 组生产事件/协议回归各 100 轮通过；`sh scripts/test-reader-ble-selection.sh` 13 项通过；`cargo +1.98 test --offline --manifest-path rust/Cargo.toml -p proletariusd reader_esim_download::tests` 45 项通过，其中包含 BLE profile 切换、BLE 下载桥绑定/失败语义、profile 概览/改名/删除等回归。两个定向 BLE 下载桥与 profile 切换测试也通过。
- 这些是合成外围与离线 LPA 测试，不代表真实 9eSIM 的 Switch 下载成功。当前系统蓝牙清单没有显示已连接的 B1；本次没有在 Switch 上读实体卡 EID/Profile、切换 profile 或下载安装，也没有使用旧 `6982` 测试码。真实下载仍须用当前有效且非 `6982` 的测试 profile 进行设备验收。

## 待设备确认

用新构建的 Switch 对 B1 依次核对连接认证、EID/Profile 读取、切卡、有效测试 profile 下载及运营商通知闭环、断线重连。若实际设备名称没有以六位十六进制标识结尾，仍需补充明确的设备身份来源；非默认口令可通过蓝牙设备列表的钥匙入口设置。真实 BLE MTU/分片节奏和长时间 keepalive 仍需 Switch 实机观察。

协议适配来源与 MIT 版权通知见 [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md)。
