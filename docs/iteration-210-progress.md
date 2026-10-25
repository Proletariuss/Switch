# Iteration 210：移植 9eSIM B1 NBLE 蓝牙传输

日期：2026-10-09。只在 `/Users/proletarius/Desktop/Switch/` 中改动；`/Users/proletarius/Desktop/Proletarius-iOS` 只读参考。

## 变更

- 在 `BluetoothReaderManager` 增加 NUS 专用 B1 协议类型，基于 Nordic UART Service/Write/Notify UUID 选择传输。B1 连接不再错误复用 AE30/BeeSIM framing；原 AE30 BeeSIM、ESTKme CCID、legacy BLE 路径继续独立。
- 实现设备名六位十六进制标识、默认口令与 16 字节密钥派生；用 CommonCrypto 做两密钥 DES-EDE ECB challenge-response。认证 ACK、eUICC 模式响应都成功后才发布连接状态。
- APDU 使用独立计数、每 100 字节 body 分片、5-bit sequence/末帧位、16-bit additive checksum；响应按序重组并验证计数、校验和，迟到响应不交给当前 APDU。
- 加入空闲 keepalive。B1 reset 命令尚无验证，因此明确返回 unsupported，不发送旧 BeeSIM reset frame。
- Rust LPA仍通过现有 BLE APDU bridge 使用此传输；Profile 启用已有 `refreshFlag=false` 行为保持不变。添加 NekokoLPA2 MIT 归属说明。

## 验证

- `BLE_MANAGER_ITERATIONS=100 sh scripts/test-ble-manager.sh`：30 组生产 BLE 状态机/协议回归 × 100 轮全部通过。
- `sh scripts/test-reader-ble-selection.sh`：13 项通过。
- 新增合成测试覆盖默认密钥派生、独立 3DES challenge-response 向量、请求分片和边界、response checksum、迟到计数、认证与 eUICC 模式连接门槛、B1 reset 防误发。
- `scripts/build.sh`：Switch 2.93/build 293 Release arm64 构建成功；`codesign --verify --deep --strict ../dist/Switch.app` 通过。Xcode 有既有 macOS 27 弃用 API 警告；此次 `BluetoothReaderManager.swift` 无编译错误。
- `git diff --check` 通过。

用户提供 `/Users/proletarius/Desktop/Proletarius-iOS` 作为只读参考，并最新确认该项目的 B1 连接、EID/Profile 读取和 Profile 切换均通过真机验证。参考目录内的 `IOS_VERIFICATION.md` build 597 记录仍称切卡待复测，文档晚于用户本次确认。Switch 自身尚未连接实体 B1 复验；本轮没有在 Switch 上读卡、写卡、切卡或发送短信。未生成 DMG。

这是 Swift CoreBluetooth 系统桥的兼容能力，不改变 Rust 后端迁移计分。全 Rust 目标总体仍为 **31.4%（14.920/47.5计划人周等值）**，估算误差约 ±3 个百分点。
