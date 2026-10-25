# Switch 2.96：9eSIM 认证复核与发行记录

日期：2026-10-10。用户明确要求核对蓝牙密码认证、B0 加密认证并输出 DMG；本轮兼容改动及产物均位于 Switch。参考 ZIP 只读，没有访问真实卡、发送短信或使用历史下载码。

## 代码结论

- B1/NBLE 使用既有默认密码派生、两密钥 DES-EDE challenge-response、认证 ACK 与 eUICC 管理模式门槛。新增蓝牙设备列表钥匙入口，按物理 CoreBluetooth UUID 在本机钥匙串保存自定义密码；自定义密码优先，恢复默认只删除该设备记录，下次连接生效。
- 明确认证拒绝和钥匙串读取失败停止自动重连，不自动尝试其他密码。Swift 私有 APDU/reset 桥在未指定 reader 的兼容请求中也必须等待认证与卡模式确认，指定 reader 时仍验证相同物理设备。
- B0 在既有 Rust 生产下载流程中，按已知 EID 范围获取/缓存每卡 PSK，在本次 GetEUICCChallenge 与 AuthenticateServer 之间发送 BF71。算法、随机数长度、TLV 和 APDU 参数与用户提供的 Clarivate 615 摘录一致；只有 9000 继续标准 LPA 下载。PSK 与蓝牙密码使用独立钥匙串账户。
- B0 摘录的设备验证背景是 USB-C 读卡器。它证明可复核的 B0 下载认证实现来源，不是全部 B0 蓝牙硬件的验证记录；B1/NBLE 与 B0 BF71 属于不同认证层。

## 验证

- Swift BLE 生产事件状态机：34 组通过，涵盖自定义密码进入真实握手、密码拒绝不重试、钥匙串失败不回退、握手/模式确认前外部 APDU 禁止进入。密码读取在测试中注入替身，不访问用户钥匙串。
- 读卡器手动选择：13 项通过。
- Rust LPA/eUICC 套件通过；LPA 钩子顺序与失败取消、B0 逻辑通道 APDU 均有回归。
- Rust daemon 完整套件在 B0 测试 helper 提取前为 1212 通过、3 忽略、0 失败；提取 helper 后 B0 定向 8 项全部通过，涵盖独立向量、精确 APDU、9000、6982、不完整响应、传输错误及无效输入不触达传输。未将未重跑的全部套件计为新结果。
- Release 构建完成；只读挂载 DMG 核验应用版本 2.96、构建 296、bundle ID `com.proletarius.switch`、最低 macOS 27.0、arm64。`codesign --verify --deep --strict` 通过（本地 ad-hoc 签名，不代表 Apple 公证），镜像校验通过，发行文件与归档的 SHA-256 相同。
- 产物：`/Users/proletarius/Desktop/Switch/dist/Switch-2.96-macOS27.dmg`，52,264,489 字节。SHA-256：`20c056578045e85d09d500b19a31fc63e6312f746dc87f49fe4b511aa95ef422`。发行 manifest 已同步版本、构建、大小及摘要。

上述为源码与合成验证，不宣称新构建已完成真卡 BF71 或完整下载，不宣称所有 6982 均已解决。全 Rust 后端迁移目标及加权进度不因这次兼容发行增加。
