---
AIGC:
  ContentProducer: '001191110102MAD55U9H0F10002'
  ContentPropagator: '001191110102MAD55U9H0F10002'
  Label: '1'
  ProduceID: '7f26fb68-cdbc-49f1-91cb-ca28708031f6'
  PropagateID: '7f26fb68-cdbc-49f1-91cb-ca28708031f6'
  ReservedCode1: '6f53a3e7-7308-4fe0-90d1-e86e5d3123f4'
  ReservedCode2: '6f53a3e7-7308-4fe0-90d1-e86e5d3123f4'
---

# Proletarius 基于 Rust 重构的架构迁移路径审议报告

> **历史评审，已过时，不作为 Switch 当前实施计划。** 原文以 2026-09-18 代码为依据，包含已移除的传真、过时的协议实现状态和迁移百分比。当前目标见 [协议加固与可用性目标](docs/optimization/2026-10-03-protocol-hardening-goal.md)。Rust 已有真实 IKE 建链、ESP、受保护 IMS 注册/连接机制，但生产接管门槛仍未满足；原文的“立即迁移”不能代替当前所有权、N待调度与实机验收。

- **日期**：2026-09-18
- **审议人**：Rust 架构师视角
- **性质**：只读审议，不修改任何文件
- **基线版本**：workspace 0.105.0，ownership schema v2，rust_ownership 60%（9/15 native，5 go-blocking）

---

## 1. 迁移范围分类

### 1.1 可以立即迁移到 Rust（Rust 已有基础或无复杂外部依赖）

| Go 模块 | 行数 | 迁移依据 |
|---|---|---|
| `reader_manager.go` — USIM 信息缓存（`usimInfo` / `loadUSIMInfoLocked` / `USIMInfo` / `RefreshUSIMInfo`） | ~200 | Rust 已有 `proletarius-reader::read_identity_auto()`，一次 PC/SC 事务完成 EF-DIR AID 发现 + ICCID + IMSI + MNC 读取。Go 端当前已在 `loadUSIMInfoLocked` 中通过 `PROLETARIUS_CARRIER_SOCKET` 回调 Rust 做 ICCID 验证。将完整的 identity 读取迁到 Rust 只是移除"先 Go 读 → 再 Rust 验证"的双路径 |
| `reader_manager.go` — AKA 鉴权（`authenticateCore` / `authenticateEAPAKA` / `authenticateISIMAKA` / `AuthenticateGSM`） | ~180 | Rust 已有 `proletarius-reader::authenticate_aka_auto()`（USIM/ISIM 自动 AID 发现 + 3G AKA），Go 端已通过 `authenticateRuntimeReaderAKAAuto` 回调 Rust。GSM 鉴权（`AuthenticateGSM`）仅需在 `proletarius-sim` 中增加一个 `authenticate_gsm_apdu()` 构造器（INS=0x88 P2=0x80），工作量极小 |
| `reader_manager.go` — 读卡器枚举与 ReconcilePresence 逻辑 | ~100 | Rust 已有 `PcscReaderScanner` + `ReaderDiscovery::reconcile()`。Go 端 `ReconcilePresence` 的 BLE 分支（grace period + bleBusy）需要在 Rust 侧增加 BLE reader awareness，但核心逻辑可迁 |
| `reader_ccid_persistent.go` — 持久 PC/SC 会话 | 103 | 纯 `pcsc` crate 操作。Rust 的 `PcscSession` 已有 `connect/transaction/close`。持久会话只是一个 `Mutex<Option<PcscSession>>` + exchange-on-demand + revival 冷却 |
| `reader_vowifi_transport.go` — SOCKS5 UDP relay 工厂 | ~100 | Rust 已有 `proletarius-vowifi::Socks5UdpRelay` + `Socks5Probe`。Go 端 `readerSWUProxyTransport` 的 readLoop/IKE/ESP 分流逻辑可直接移植到 Rust |
| `estkble/framing.go` — BLE APDU 帧格式 | 62 | 纯二进制编解码，无平台依赖。`APDURequest`/`Reply` 可直接用 Rust 重写（Little-Endian u16 length + command byte） |
| `estkble/channel.go` — ESTKme BLE 通道适配器 | 103 | `Channel` 的 `OpenLogicalChannel`/`CloseLogicalChannel`/`Transmit` 逻辑是纯 APDU 协议。Rust 侧只需一个 `estkble::Channel` 实现 `proletarius-sim::ApduTransport` trait |

### 1.2 需要中等努力迁移（需要找 Rust 等价库或移植协议层）

| Go 模块 | 行数 | 挑战与 Rust 等价物评估 |
|---|---|---|
| `reader_vowifi_session.go` — VoWiFi 会话编排（SWu + IMS 启动/停止/状态机） | 883 | Rust 已有策略层（`select_proxy_mode`、`ProxyMode`）、IKE 编解码器（`proletarius-ike`）、IMS 协议校验器（`proletarius-ims`），但缺少 **SWu 会话状态机**（IKE_SA_INIT → IKE_AUTH → CHILD_SA）和 **IMS 注册状态机**（REGISTER 401 → Security-Server → REGISTER 200）。Go 端使用 `swu-go` 的 `Session` + `sipgo` 的对话层。Rust 需要从头实现这两个状态机，但协议核心编解码已就绪 |
| `estkble/client.go` — BLE bridge Unix socket 客户端 | 142 | 纯 JSON-over-Unix-socket 客户端，Rust 用 `tokio::net::UnixStream` + `serde_json` 可直接移植。但 `Client` 的 `request` 模型（一请求一连接）需要适配 tokio async |
| `reader_vowifi_transport.go` — 直接 UDP 传输 + IP_BOUND_IF | ~80 | `ipsec.NewSocketManager` 来自 `swu-go`。Rust 侧需要用 `tokio::net::UdpSocket` + `nix::sys::socket::setsockopt`（IP_BOUND_IF / IPV6_BOUND_IF）重写 |
| `vowifi/simauth/` — RFC 3310 Digest AKAv1-MD5 | 246 | 纯 MD5 + hex + base64 计算。Rust 用 `md-5` + `base64` + `hex` crate 可直接实现。但需要仔细对齐 RFC 3310 的 nonce 编码和 auts 指令语义 |
| `vowifi/carrier/` — 运营商画像注册表 | ~2000 | 嵌入式 JSON + Go 结构体。Rust 用 `serde` + `phf`（编译期静态 map）或 `serde_json::from_str` 可移植。数据量不大但字段多，需逐一对齐 |
| `reader_esim_api.go` — eSIM HTTP 端点（overview/switch/download/rename/delete/notifications） | 620 | HTTP 层可迁移（hyper handler），但底层依赖 `esim.Manager` → `euicc-go` 的 LPA，这是核心瓶颈（见 §7） |
| `esim/identity.go` / `euicc_info.go` — eUICC 信息读取 | ~400 | 底层是 SGP.22 APDU（ES10b/ES10c），通过 `euicc-go` 的 BERTLV 编解码。Rust 生态有 `bertlv` crate 但不完整；需要自行实现 SGP.22 的 ASN.1 编解码子集 |

### 1.3 暂不建议迁移（依赖根深蒂固的 Go 生态、或 macOS 平台限制）

| Go 模块 | 行数 | 不迁移原因 |
|---|---|---|
| `vowifi/imscore/` — IMS SIP 运行时（REGISTER/INVITE/MESSAGE/通话/传真） | ~6000+ | 依赖 `sipgo`（完整的 SIP UAC/UAS 实现）。Rust 生态没有生产级 SIP 栈。`proletarius-ims` 仅做协议校验（REGISTER 状态机 + header 解析），不处理实际 SIP 对话。移植 sipgo 的全部功能到 Rust 是一个独立的大型项目 |
| `vowifi/ipsec3gpp/` — 3GPP IPsec 数据面（ESP 加解密 + gvisor netstack） | ~2000 | 依赖 `swu-go` 的 `ipsec` 包 + gvisor 用户态网络栈。Rust 生态有 `ipsec`/`aes-gcm` crate，但 gvisor 的用户态 TCP/IP 栈没有 Rust 等价物。整个 VoWiFi 数据面基于 gvisor 的单一 netstack 会话设计是架构核心约束 |
| `internal/modem/` — DJI 模块 USB/QMI/MBIM 通信 | ~5000+ | 依赖 `quectel-qmi-go`（vendored + macOS libusb transport）、`goscard`（PC/SC）、`go.bug.st/serial`。Go 在 USB 硬件 I/O 生态成熟，Rust 的 `rusb` + `serialport` 可行但 QMI/MBIM 协议栈移植量大 |
| `esim/manager.go` — euicc-go LPA 会话管理 | 3789 | 依赖 `euicc-go` 的完整 SGP.22 LPA 实现（ES9+ HTTP、ES10b/ES10c APDU、BERTLV、PKI 证书链验证）。这是一个专用的 Go 库，Rust 生态没有等价物（详见 §7） |
| `esim/qmi_channel.go` / `mbim_apdu_transport.go` — 模块 eSIM APDU 通道 | ~600 | QMI UIM / MBIM APDU 传输是 Go 硬件 I/O 层的一部分，与模块管理深度耦合 |
| `vowifi/voiceclient/` — SIP 语音客户端 | ~3000+ | 依赖 sipgo + gvisor netstack + CoreAudio bridge（通过 Swift IPC） |

---

## 2. 依赖分析

| Go 依赖 | 用途 | Rust 等价物 | 评估 |
|---|---|---|---|
| `pcsc` (via `goscard`) | PC/SC 智能卡接口 | `pcsc = "2.9"` | ✅ **已有**。Rust `pcsc` crate 直接绑定 macOS PC/SC framework，`proletarius-reader` 已在用 |
| `euicc-go` | SGP.22 eUICC LPA（ES9+/ES10b/ES10c、BERTLV、PKI） | 无直接等价 | ❌ **缺失**。详见 §7 评估 |
| `uicc-go` | USIM 文件系统读取（EF-DIR、EF-IMSI 等） | 部分自研 | ⚠️ **部分**。`proletarius-sim` 已实现 SELECT AID、READ BINARY、EF-DIR 遍历、IMSI/ICCID 解码、AKA AUTHENTICATE。缺 EF-MSISDN/EF-SPN/EF-PNN/EF-AD 读取（已在 `proletarius-sim` 中有 `decode_msisdn_record`/`decode_smsp_record`，需补充 SELECT + READ 调用链） |
| `swu-go` | VoWiFi SWu 会话（IKEv2/EAP-AKA）、gvisor netstack、IPsec | 无直接等价 | ❌ **缺失**。Rust 有 `proletarius-ike`（IKE 编解码）和 `proletarius-vowifi`（SOCKS5/代理策略），但缺 IKEv2 状态机 + EAP-AKA 状态机 + gvisor 等价 |
| `sipgo` | SIP UAC/UAS 对话管理 | 无生产级等价 | ❌ **缺失**。Rust 生态有 `pjsip` 绑定（C 库）但不是纯 Rust。自研 SIP 栈是独立的大型项目 |
| `gvisor` | 用户态 TCP/IP 网络栈 | 无等价 | ❌ **缺失**。这是 VoWiFi 的架构约束：整个会话共享单一 netstack。Rust 的 `smoltcp` 是一个轻量 TCP/IP 栈但 API 模型完全不同，不支持 gvisor 的 file system / fd 模型 |
| `icholy/digest` | RFC 2617 HTTP Digest 认证 | `http-auth` / 自研 | ⚠️ **可替代**。Rust 有 `http-auth` crate 支持 Digest，但 AKAv1-MD5 需要自定义 algorithm 标签（与 Go 端一样的 workaround） |
| `google/uuid` | UUID 生成 | `uuid` crate | ✅ **等价** |
| `viper` | 配置管理 | `serde` + `toml`/`json` | ✅ **等价**。Rust 配置模式更简单（直接反序列化） |
| `warthog618/sms` | SMS PDU 编解码 | `proletarius-sms` crate | ✅ **已有**。Rust 端已自研 RP-DATA / TPDU 编解码 |
| `golang.org/x/sync/singleflight` | 请求去重 | `tokio::sync::Mutex` + 自研 | ✅ **可替代**。模式简单，Rust 可用 `OnceCell` + `Mutex` 实现 |

---

## 3. 迁移顺序建议

### 阶段 0：已完成（当前基线）

Rust 已拥有：
- 读卡器枚举（`PcscReaderScanner`）、identity 读取（`read_identity_auto`）、AKA 鉴权（`authenticate_aka_auto`）
- IKE/IMS/SMS/传真 协议编解码
- 代理策略选择、SOCKS5 探测、传输选择
- 网关/认证/进程监督/归档存储

### 阶段 1：读卡器会话层迁移（低风险，高收益）

**目标**：将 reader_session 从 `go/protocol-shadow` 升级为 `rust/native`

| 子任务 | 依赖 | 可并行 |
|---|---|---|
| 1a. Rust USIM 文件读取补全（EF-MSISDN/EF-SPN/EF-PNN/EF-AD/EF-GID2） | `proletarius-sim` 已有基础 | 是 |
| 1b. Rust 持久 PC/SC 会话（`PersistentCcidSession`） | `pcsc` crate | 是 |
| 1c. Rust BLE 帧编解码（`estkble::framing` + `estkble::channel`） | 无 | 是 |
| 1d. Rust BLE bridge Unix socket 客户端（`estkble::client`） | tokio | 依赖 1c |
| 1e. Rust GSM 鉴权（`authenticate_gsm_apdu`） | `proletarius-sim` | 是 |
| 1f. Rust reader session 状态机（`ReaderSession`：连接/断开/重建/能力判定） | 1a-1e | 否（汇总） |
| 1g. Rust HTTP 端点注册（`/api/v2/reader/session`、`/api/v2/reader/usim-info`） | 1f | 否 |

**P0 问题消除**：
- P0-1（RebuildSession 锁窗口）→ Rust 用 `tokio::sync::Mutex` + `async fn rebuild()` 一步完成，无锁释放窗口
- P0-3（AB-BA 死锁）→【2026-10-03 纠正】`Send + Sync` 不阻止锁序倒置。需要统一资源持有者或明确锁顺序，并用并发/取消测试验证。
- P0-2（isESTKmeReader 恒 false）→ 见 §6

### 阶段 2：VoWiFi 传输与会话编排迁移（中等风险）

**目标**：将 reader_vowifi 的传输构造层迁到 Rust，IMS/SIP 运行时保留在 Go

| 子任务 | 依赖 | 可并行 |
|---|---|---|
| 2a. Rust SOCKS5 UDP relay 传输实现 | `proletarius-vowifi` 已有 codec | 是 |
| 2b. Rust 直接 UDP 传输 + IP_BOUND_IF | `tokio::net::UdpSocket` + `nix` | 是 |
| 2c. Rust IKE/ESP 包分流（`readerIKEPayload` 逻辑） | `proletarius-ike` | 是 |
| 2d. Rust VoWiFi 会话编排外壳（启动/停止/状态查询） | 2a-2c | 否 |
| 2e. ownership.rs 更新：`reader_vowifi` transport → `rust/native-transport` | 2d | 否 |

**注意**：SWu IKEv2 状态机和 IMS 注册状态机仍保留在 Go 端（通过 `swu-go` + `sipgo`）。Rust 侧的会话编排外壳通过 IPC 向 Go 发出"启动 SWu"和"启动 IMS"指令，Go 返回隧道结果和注册状态。

### 阶段 3：eSIM 管理层迁移（高复杂度，需独立决策）

详见 §7。此阶段需要先决定 LPA 策略，然后才能确定迁移路径。

### 阶段 4：长期演进（不在本次审议范围）

- IMS SIP 运行时迁移（需要 Rust SIP 栈）
- gvisor netstack 替代（需要评估 smoltcp 或自研）
- DJI 模块 USB/QMI/MBIM 迁移

---

## 4. 双运行期策略

### 4.1 现有 route_owner 机制

`ownership.rs` 中的 `route_owner(method, path)` 函数是一个静态路由表，按 HTTP method + path 决定请求由 Rust 处理还是转发给 Go。当前状态：

```
Rust native:     gateway, reader_inventory, reader_identity, sms_codec, call_archive, fax_*, message_archive
Rust shadow:     device_snapshot (聚合读，部分字段来自 Go)
Go compatibility: dji_module, reader_session, reader_vowifi, message_runtime
Swift native:    mac_audio
```

### 4.2 渐进迁移支持的改进建议

**当前机制足够支持阶段 1-2 的渐进迁移**，但需要以下补充：

1. **新增协议契约**：`reader_session_contract = "reader-session-v1"`——Rust 接管 reader session 后需要向 Swift 暴露统一的 session 事件流（连接/断开/重建/能力变更）。当前 Go 通过 `/api/reader/vowifi` 的轮询返回 `readerStatus`；Rust 接管后应保持相同的响应 schema，使 Swift 无感知。

2. **路由切换粒度**：当前 `route_owner` 是全路径匹配。阶段 1f 完成后，需要将以下路由从 `GoCompatibility` 切换到 `Rust`：
   - `POST /api/reader/connect` → Rust
   - `POST /api/reader/disconnect` → Rust
   - `POST /api/reader/refresh` → Rust
   - `GET /api/reader/vowifi`（reader status 部分）→ Rust

3. **双写期**：迁移 reader_session 时，Rust 和 Go 可能同时持有 PC/SC handle。当前 `reader_auth_lock(reader)` 已按读卡器名称互斥，但这是进程内的。跨进程互斥需要：
   - 方案 A：Rust 接管后 Go 端的 reader_manager 退化为"不持有任何 PC/SC handle，仅转发请求"的薄壳
   - 方案 B：在 Rust 端实现一个跨进程的 PC/SC 排他锁（通过 Unix socket lock server 或 flock 文件锁）

   **推荐方案 A**：Go 端 `readerManager` 改为 RPC 客户端，所有 PC/SC 操作通过 IPC 委托 Rust。这避免了跨进程锁的复杂性，且与 ownership.rs 的 "protocol-shadow → native" 升级路径一致。

4. **回退机制**：`route_owner` 是编译期静态表。建议增加一个运行期特性开关（环境变量 `PROLETARIUS_READER_NATIVE=1`），在验证期间允许快速回退到 Go 路径。

### 4.3 ownership.rs 升级路径

```rust
// 阶段 1f 完成后
MigrationComponent {
    id: "reader_session",
    owner: "rust",           // 从 "go" 变为 "rust"
    state: "native",         // 从 "protocol-shadow" 变为 "native"
    requires_go_runtime: false,  // 从 true 变为 false
    blocks_go_removal: false,    // 从 true 变为 false
    detail: "Rust owns reader session lifecycle, USIM identity, AKA auth, \
             BLE channel framing and persistent CCID sessions",
},
```

这会将 `rust_ownership_percent` 从 60% 提升到 ~73%（11/15），`go_blocking_components` 从 5 降至 3。

---

## 5. Go 的 P0 问题在 Rust 重构后的消解

### P0-1: RebuildSession 锁释放窗口与 ReconcilePresence 竞态

**Go 问题本质**：`RebuildSession` 在 `rm.mu.Unlock()` → `powerCycle()` → `rm.mu.Lock()` 之间释放了锁，`ReconcilePresence` 可以在此窗口内调用 `disconnectLocked()` 清空 `readerName`，重建后用捕获的旧 `readerName` 覆盖。

**Rust 消解**：

```rust
// 历史示意：持锁 across await 不意味着业务事务原子性或无死锁。
async fn rebuild_session(&self, power_cycle: impl AsyncFnOnce() -> Result<()>) -> Result<()> {
    let mut session = self.session.write().await;  // 持锁
    session.teardown().await;                       // 不释放锁
    if let Err(e) = power_cycle().await { ... }     // 在锁内执行 power cycle
    session.rebuild().await?;                       // 原子重建
    // 锁在整个 rebuild 期间不释放
    Ok(())
}
```

【2026-10-03 纠正】持有同一把锁可串行进入该临界区，但 `async fn` 不提供业务事务原子性；跨硬件等待持锁还可能造成取消、锁序及活性问题。应按真实调用链设计单一持有者、代次检查和完成后释放规则，不能按此伪代码直接接管。

### P0-2: isESTKmeReader("BLE:UUID") 恒 false

**Go 问题本质**：`isESTKmeReader` 用 `strings.Contains(strings.ToLower(name), "estkme")` 判定，但 BLE 读卡器的 PC/SC 名称是 `"BLE:<UUID>"`，不含 "estkme" 字符串。

**Rust 消解**：见 §6 详细分析。

### P0-3: rm.mu ↔ rm.authMu 锁序倒置（AB-BA 死锁）

**Go 问题本质**：
- `loadUSIMInfoLocked`：持 `rm.mu` → 获取 `rm.authMu`（A→B）
- `authenticateCoreAtGeneration`：持 `rm.authMu` → 获取 `rm.mu`（B→A）

**Rust 消解**：

【2026-10-03 纠正】`Send + Sync` 与 borrow checker 不会自动检查 AB-BA 锁序。单一锁持有或明确层次化锁可以降低风险，但其正确性来自设计和验证，而非换用 Rust 后自动成立：

```rust
// 方案：将 reader session 状态收敛到一把 async 锁
struct ReaderSession {
    inner: tokio::sync::Mutex<SessionState>,
    // auth 操作在 session 锁内执行，不需要第二把锁
}

struct SessionState {
    reader_name: String,
    generation: u64,
    cached_usim: Option<UsimInfo>,
    // auth 操作的串行化通过 session 锁本身完成
}
```

【2026-10-03 纠正】不同锁的 guard 约束不同，Tokio guard 可跨 await；即使某 guard 不是 Send，也不保证单线程任务不会死锁。标准库 Mutex 没有上述自动死锁检测保证，第三方可选诊断器也不能替代锁序和取消验证。参见 [Rust 官方说明](https://doc.rust-lang.org/book/ch16-03-shared-state.html) 与 [Tokio Mutex](https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html)。

---

## 6. BLE 读卡器的特殊处理

### 6.1 核心约束

CoreBluetooth **必须在 Swift 主进程**运行（macOS 限制：CBCentralManager 需要 RunLoop）。Rust 进程无法直接访问 CoreBluetooth。当前架构：

```
Swift (BLEBridge) ← JSON over Unix socket → Go (estkble.Client) → estkble.Channel → euicc-go
```

### 6.2 Rust 接管后的 BLE bridge 协议

**现有协议不需要调整**。BLE bridge 的 JSON IPC 协议（`bridgeRequest`/`bridgeResponse`）是传输无关的：

```json
// Request
{"token":"...", "action":"apdu", "hex":"00A404...", "reader":"..."}
// Response
{"ok":true, "hex":"9000"}
```

Rust 端的 `estkble::Client`（待实现）直接复用相同的 JSON schema 和 Unix socket 路径。Swift 端 BLEBridge 无感知后端从 Go 切到 Rust。

### 6.3 P0-2 的 Rust 解法

**Go 问题**：`isESTKmeReader("BLE:UUID")` 返回 false，因为 BLE 读卡器名称不含 "estkme"。

**Rust 解法**：Rust 不依赖读卡器名称字符串匹配，而是在 `ReaderSession` 中维护一个 `ReaderCapability` 枚举：

```rust
#[derive(Clone, Debug)]
enum ReaderCapability {
    /// USB CCID ESTKme reader — supports refresh=true power cycle
    EstkmeCcid,
    /// BLE reader connected via Swift bridge — power cycle via bridge reset
    EstkmeBle,
    /// Generic CCID reader — no software reset, must physically re-seat
    GenericCcid,
    /// 9eSIM / SimLink / other BLE reader
    OtherBle,
}

struct ReaderSession {
    name: String,
    capability: ReaderCapability,
    // ...
}
```

能力判定在连接时一次性确定，来源包括：
1. **PC/SC 名称前缀**：`"BLE:"` → BLE reader
2. **BLE bridge status 查询**：Swift 端 BLEBridge 在 `status` 响应中返回设备名（`response.Reader`），其中包含设备型号（如 "ESTKme Red"、"9eSIM250099"）
3. **ATR 匹配**：已知 ESTKme 读卡器的 ATR 模式

这避免了 Go 端 `isESTKmeReader` 纯字符串匹配的脆弱性，且 Rust 的 `ReaderCapability` 是一个强类型枚举，不会出现"BLE 读卡器被判为 Generic CCID"的类型混淆。

### 6.4 Rust BLE channel 实现

```rust
// estkble crate (待创建)
pub struct Channel {
    client: Client,           // Unix socket BLE bridge client
    current_channel: Option<u8>,
}

impl ApduTransport for Channel {
    type Error = BleError;
    
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
        // 1. 帧封装：[0x04, len_le_16, apdu...]
        let frame = apdu_request(command)?;
        // 2. 通过 BLE bridge 发送
        let response = self.client.exchange(&frame)?;
        // 3. 帧解封
        let payload = parse_reply(&response)?;
        Ok(payload)
    }
}
```

Rust 的 `ApduTransport` trait 使 BLE channel 可以无缝替代 PC/SC channel，LPA 层（无论在 Rust 还是 Go）只看到 `ApduTransport` 接口。

---

## 7. eSIM LPA 层的核心挑战

### 7.1 问题陈述

`euicc-go`（`github.com/damonto/euicc-go`）是一个完整的 SGP.22 LPA 实现，包含：
- ES9+ 客户端（HTTP JSON 与 SM-DP+ 服务器交互）
- ES10b/ES10c APDU 命令（eUICC 本地管理）
- BERTLV 编解码
- PKI 证书链验证（SGP.22 RSP 架构）
- Profile 下载/安装/启用/禁用/删除/重命名
- Notification 管理

`esim/manager.go`（3789 行）在此基础上增加了：
- 多 AID 扫描策略（eSTK.me SE0/SE1、5ber.eSIM、XeSIM、LinksField 等）
- 运营商特定行为（refresh=true/false 按读卡器能力决定）
- 下载进度回调
- 错误分类（物理 SIM vs eUICC）

### 7.2 三种方案评估

#### 方案 A：在 Rust 中从头实现 SGP.22 LPA 协议

| 维度 | 评估 |
|---|---|
| 工作量 | **极大**。SGP.22 规范约 300 页，涉及 ES10a/ES10b/ES10c 三个函数族、ES9+ HTTPS 交互、ASN.1/BER-TLV 编解码、PKI 证书链验证。保守估计 3-6 个月 |
| 风险 | **高**。SGP.22 协议细节复杂（如 profile 下载的 BPP 解析、eUICC 签名验证），实现错误可能导致锁卡。euicc-go 经过多版本迭代和社区测试 |
| 收益 | 完全消除 Go 依赖，Rust 拥有完整 eSIM 栈 |
| 建议 | **不推荐**作为近期目标。可作为长期研究方向，但不应阻塞 reader_session 和 VoWiFi 传输层的迁移 |

#### 方案 B：保留 Go 作为 eSIM APDU 执行器，Rust 管理会话状态

| 维度 | 评估 |
|---|---|
| 架构 | Rust 端拥有 `ReaderSession`（读卡器连接/断开/能力判定/USIM 读取/AKA 鉴权），Go 端仅保留 `esim.Manager`（LPA 会话），通过 `driver.SmartCardChannel` 接口接收 APDU 传输。Rust 提供 BLE channel 和持久 CCID channel 作为 `SmartCardChannel` 实现 |
| 工作量 | **中等**。需要在 Rust 侧实现 `estkble::Channel`（满足 Go 的 `driver.SmartCardChannel` 接口）和持久 CCID exchanger，然后通过 IPC 暴露给 Go。实际上 Go 端已经有 `estkble.Channel` 适配 `driver.SmartCardChannel`，只是传输层从"Go 直连 BLE bridge"变为"Rust 提供 APDU transport，Go 通过 IPC 调用" |
| 风险 | **低**。LPA 逻辑不变，APDU 传输层是薄适配 |
| 收益 | Rust 拥有读卡器会话控制权（解决 P0 问题），Go 保留经验证的 LPA 逻辑 |
| 问题 | 仍需 Go 运行时做 eSIM 操作。但这与 MEMORY.md 中"务实路线：接受双后端架构为长期稳定形态"一致 |
| 建议 | **推荐**。这是阶段 3 的首选方案 |

#### 方案 C：寻找其他语言的 LPA 库并通过 IPC 桥接

| 维度 | 评估 |
|---|---|
| 候选 | NekokoLPA2（Rust + Flutter，`github.com/iebb/NekokoLPA2`）的 Rust 底层有 SGP.22 LPA 实现。可以评估是否可提取其 Rust LPA 模块 |
| 工作量 | **中等偏高**。需要评估 NekokoLPA2 的 Rust LPA 模块的完整性、许可证兼容性、API 适配成本 |
| 风险 | **中**。依赖第三方 Rust 库的维护质量和协议覆盖度 |
| 收益 | 如果可用，可实现纯 Rust eSIM 栈 |
| 建议 | **值得调研**，但不应作为阶段 3 的唯一路径。建议先执行方案 B，同时调研 NekokoLPA2 的 Rust LPA 模块 |

### 7.3 推荐策略

**阶段 3a**：执行方案 B——Rust 提供 APDU 传输层，Go 保留 LPA 逻辑。

具体实现：
1. Rust 侧实现 `estkble::Channel`（BLE APDU transport）和 `PersistentCcidExchanger`（持久 PC/SC）
2. 暴露一个 Rust HTTP 端点 `/api/v2/reader/apdu-transport`，接收 APDU hex，返回响应 hex
3. Go 端 `esim.Manager` 的 `SmartCardChannelFactory` 改为调用 Rust 的 APDU transport 端点，而不是直接操作 PC/SC 或 BLE bridge
4. 这使 Go 的 `euicc-go` LPA 逻辑完整保留，但 PC/SC / BLE 的物理 I/O 控制权在 Rust

**阶段 3b**（长期）：调研 NekokoLPA2 Rust LPA 模块（方案 C），如果可用则逐步替换 Go LPA。

---

## 8. 迁移路线图框架

```
阶段 0 (已完成)
  └─ Rust: gateway, reader_inventory, reader_identity(read-only), 
           AKA auth, SOCKS5/proxy policy, IKE/IMS/SMS/fax codec

阶段 1 (1-3 周)
  ├─ 1a: proletarius-sim 补全 USIM 文件读取 (EF-MSISDN/SPN/PNN/AD/GID2)
  ├─ 1b: proletarius-reader 增加 PersistentCcidSession
  ├─ 1c: 新建 estkble crate (framing + channel)
  ├─ 1d: estkble::Client (BLE bridge Unix socket client)
  ├─ 1e: proletarius-sim 增加 authenticate_gsm_apdu
  ├─ 1f: proletarius-reader 增加 ReaderSession 状态机
  ├─ 1g: proletariusd 注册 reader session HTTP 端点
  └─ ownership.rs: reader_session → rust/native
     ↳ P0-1, P0-2, P0-3 自然消解
     ↳ rust_ownership 60% → 73%

阶段 2 (2-4 周，与阶段 1 部分并行)
  ├─ 2a: proletarius-vowifi 增加 SOCKS5 UDP relay 传输
  ├─ 2b: proletarius-vowifi 增加直接 UDP 传输 + IP_BOUND_IF
  ├─ 2c: proletarius-ike 增加 IKE/ESP 包分流
  ├─ 2d: proletariusd 增加 VoWiFi 会话编排外壳
  └─ ownership.rs: reader_vowifi transport → rust/native-transport
     ↳ rust_ownership 73% → 80% (transport 层)

阶段 3a (1-2 周)
  ├─ Rust APDU transport 端点暴露
  ├─ Go esim.Manager SmartCardChannelFactory 改为 RPC
  └─ ownership.rs: reader_session eSIM 部分 → rust/transport-native
     ↳ Go 保留 LPA 逻辑，但物理 I/O 由 Rust 控制

阶段 3b (长期，可选)
  └─ 调研 NekokoLPA2 Rust LPA → 评估替换 Go LPA

阶段 4 (不在本次范围)
  └─ IMS SIP 运行时 / gvisor netstack / DJI 模块迁移
```

### 关键里程碑

| 里程碑 | 完成标志 | Go 可移除组件 |
|---|---|---|
| M1: 阶段 1 完成 | Rust 独立管理读卡器会话/identity/AKA，无 Go PC/SC 调用 | reader_session |
| M2: 阶段 2 完成 | Rust 控制 VoWiFi 传输选择和 SOCKS5 relay | reader_vowifi (transport) |
| M3: 阶段 3a 完成 | Rust 提供 APDU transport，Go LPA 通过 RPC 调用 | (Go 仍需运行 LPA) |
| **Go 移除就绪** | 需要阶段 3b + 阶段 4 | 全部 |

**重要说明**：根据 MEMORY.md 中的务实路线定位——"接受双后端架构为长期稳定形态，不追'全量重写消除 Go'的边际收益"——阶段 1-2 完成后即可消除全部 P0 问题并显著提升 Rust 占比，阶段 3a 进一步将物理 I/O 控制权归 Rust，**不需要等待完全移除 Go 即可获得架构收益**。

---

## 9. 总结

| 维度 | 结论 |
|---|---|
| **P0 问题** | 三个 P0 问题全部集中在 Go 的 `reader_manager.go` 并发设计。阶段 1 完成后通过 Rust 所有权模型 + async 锁自然消解，无需在 Go 端单独修复 |
| **迁移可行性** | 读卡器会话层（阶段 1）和 VoWiFi 传输层（阶段 2）可立即迁移，Rust 已有充分基础。eSIM LPA 层建议方案 B（Rust transport + Go LPA）作为近期路径。IMS SIP 运行时和 gvisor netstack 暂不迁移 |
| **BLE 读卡器** | JSON IPC 协议无需调整，Rust 直接复用。P0-2 通过 `ReaderCapability` 枚举 + BLE bridge status 查询解决 |
| **双运行期** | 现有 `route_owner` 机制足够支持渐进迁移。建议增加运行期开关 `PROLETARIUS_READER_NATIVE=1` 用于验证期回退 |
| **Go 移除就绪** | 阶段 1-2 完成后 `go_blocking_components` 从 5 降至 3（dji_module + reader LPA + message_runtime）。完全移除 Go 需要阶段 3b（Rust LPA）+ 阶段 4（IMS/netstack），是长期目标 |
