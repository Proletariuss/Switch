# Iteration 546：受沙箱约束的升级、降级与再升级数据往返

2026-10-18。P6 增加实际旧版二进制的可重复读写验证。Iteration 545
只验证旧 3.42 能读取当前格式；本轮使用旧 3.42 的 Rust gateway 与 Go 子进程、
从当前源码离线构建的 Rust daemon，执行四阶段业务数据副本往返。没有修改生产
Rust/Swift 代码、运营商或 VoWiFi 协议文件，没有生成 DMG。

## 隔离与硬件边界

新增 `scripts/check-p6-data-roundtrip.py`，必须显式指定当前二进制、旧版 backend
目录与报告路径。脚本只从 `native_recovery_tests.rs` 的四份字面合成 fixture
建立输入，不读取已安装应用的数据、Keychain 或卡片内容。所有阶段均在新建的
`../build/runtime/p546-*` 目录中；阶段间只复制以下四份业务文件：

- `sms-archive.json`
- `device-send-ledger.json`
- `multistandby/store-v1.json`
- `multistandby/operations-v1.json`

旧版启动会自动发现设备，因此仅配置临时数据目录不能证明无硬件接触。脚本
强制通过 macOS `sandbox-exec` 启动每个 daemon，子进程继承相同限制：禁止
全部 IOKit 打开、全部 Mach 服务查找、设备文件内容访问、阶段目录外的写入，
并禁止阶段目录外的出站 Unix socket 及 IP 连接。只允许 `/dev` 目录枚举和
设备路径元数据，允许 `/dev/null` 与系统随机数源；这些例外不允许打开串口、
USB、PC/SC 或 BLE 设备。目录枚举是旧版完成无设备分支选择所需，仍不提供
任何卡片或模块交互能力。未读取、控制或停止现有已安装 daemon。

每阶段启动前，独立探针使用 `sandbox_check` 检查 IOKit 与 Mach 禁止规则，
验证 `/dev/zero` 打开、阶段外合成 Unix socket、IPv4 连接均被拒绝，以及
阶段内 Unix socket 可连接；然后启动子进程重复全部探针以验证继承。任何探针
失败都在 daemon 启动前中止，不提供无沙箱降级路径。`scripts/tests/test_p6_data_roundtrip.py`
包含五项实际系统沙箱测试；分别移除 IOKit、Mach、设备文件或网络规则的负例
均被拒绝。负例只检查策略、`/dev/zero` 和合成 socket，不访问实体设备。

## 四阶段结果

| 阶段 | 运行时 | 归档条数前 → 后 | SIGTERM 退出 |
| --- | --- | --- | --- |
| 旧版首次读取并确认删除 | 3.42 gateway + Go，ready | 4 → 3 | 1，既有通话模式清理缺陷 |
| 当前 Rust 升级读取并确认删除 | rust-native，Go retired | 3 → 2 | 0，原生退出回执 |
| 旧版降级读取并确认删除 | 3.42 gateway + Go，ready | 2 → 1 | 1，同一既有缺陷 |
| 当前 Rust 再升级读取 | rust-native，Go retired | 1 → 1 | 0，原生退出回执 |

八次阶段前后检查均验证：已确认归档正文、请求 ID、未知扩展字段及中文/emoji
保留；先前确认的删除没有复活；发送账本只有原有两项，accepted 保持确认，
submitting 恢复为 unknown 且 `can_submit=false`，原尝试标识不变；N待接收消息
仍存在，中断发送保持 unknown，中断操作保持 unknown，已完成操作保持 completed。
未调用短信发送 API。原始回退输入 SHA-256 全程未变；报告记录三份可执行文件、
fixture 源文件、脚本、各阶段沙箱策略与四份输入/输出文件的 SHA-256。

首次成功运行之后，增加旧版版本断言、原始发送状态断言、最终二进制零漂移
检查及审计指纹，最终版本再次独立运行通过。五项沙箱测试通过；当前 Rust
使用 `cargo build --offline --locked` 构建通过。所有测试自有进程组均已退出，
没有清理或强杀其他应用进程。

证据：`build/reports/iteration-546-data-roundtrip.json`（最终运行）、
`build/reports/iteration-546-data-roundtrip-third.json`（前一次成功运行）。这些路径
均相对 `Switch_MAC` 根目录。早期沙箱禁止目录枚举时，旧版未达到 ready，
没有作为成功验收；失败保留在 `iteration-546-data-roundtrip-second.json` 与其
独立运行日志中。最终规则仍拒绝设备文件内容、IOKit 打开和 Mach 服务访问。

## 复现

从 `src` 执行，不要求 Go 编译器：

```sh
cargo build --offline --locked --manifest-path rust/Cargo.toml \
  --target-dir ../build/rust-target -p proletariusd --bin switchd-rs
python3 -m unittest scripts/tests/test_p6_data_roundtrip.py -v
python3 scripts/check-p6-data-roundtrip.py \
  --current ../build/rust-target/debug/switchd-rs \
  --legacy-backend ../build/release-3.42/Build/Products/Release/Switch.app/Contents/Resources/backend \
  --report ../build/reports/iteration-546-data-roundtrip.json
```

## 验收范围

此结果证明这四份合成业务数据在实际旧/新后端之间的副本读写兼容，包括新版
写后旧版再写和再次升级读取。阶段使用新目录，特意不复制运行锁、通话模式
原始标记、其他运行状态或其他存储类型，因此不代表在同一个旧应用数据目录
内原位升级/回退。旧 3.42 的非零退出仍阻止完整降级验收；沙箱无设备条件不
代表其真实模块退出行为已修复。

本轮没有覆盖安装/旧 UI、真实用户密钥、全部数据类型、SWF1 转发队列反向
写兼容、实体 USB/BLE/模块、运营商业务或全应用性能，也没有据此提高整体
完成百分比或将 P6 标记完成。当前二进制是 Debug 构建；运行时间不作为性能
比较。Iteration 513 的恢复/密钥测试与 Iteration 535 的 pristine 退出验证仍是
独立证据，不能由此替代。
