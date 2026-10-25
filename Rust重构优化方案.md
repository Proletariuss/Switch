---
AIGC:
  ContentProducer: '001191110102MAD55U9H0F10002'
  ContentPropagator: '001191110102MAD55U9H0F10002'
  Label: '1'
  ProduceID: 'fc63d154-a762-432f-833f-0d7f18551e16'
  PropagateID: 'fc63d154-a762-432f-833f-0d7f18551e16'
  ReservedCode1: '27cd9cd0-5e60-4722-b88f-b61e3dedc125'
  ReservedCode2: '27cd9cd0-5e60-4722-b88f-b61e3dedc125'
---

# Proletarius 读卡器 / eSIM 层 Rust 重构方案

- **日期**：2026-09-18
- **方向**：所有优化基于 Rust 重构，优先使用 Rust
- **审查方式**：三遍交叉审议（架构迁移路径 / 技术可行性与设计 / 风险与回归）+ 代码结构实勘（Rust 13 crate 68 源文件 / Go 35+ 文件 / Swift 63 文件）
- **性质**：建议方案，未修改任何代码

---

## 0. 摘要

**核心结论：基于 Rust 重构读卡器 / eSIM 层可行且有显著收益，但必须分阶段实施。**

Rust 已有 `proletarius-reader`（pcsc v2.9）和 `proletarius-sim`（APDU/AKA/ICCID decode）两个 crate 作为基础设施，且已承担只读读卡器操作（枚举 / 身份 / AKA）。当前 Rust 占比 60%（`ownership.rs`）。将读卡器会话层迁移到 Rust 后：

- **三个 P0 问题在 Rust 中自然消解**（编译期线程安全、单一 async 锁、所有权模型）
- **Rust 占比提升至 ~73%**（reader_session 从 `go/protocol-shadow` 变为 `rust/native`）
- **不触动高风险的 LPA 写操作**——采用"Rust transport + Go LPA"分层方案

**三审查共识**：

| 议题 | 三审查一致结论 |
|---|---|
| 优先迁移 | 读卡器会话层（持久 PC/SC、USIM 读取、ReconcilePresence、BLE channel） |
| LPA 策略 | 方案 B：Rust 接管 APDU 传输层，Go 保留 euicc-go LPA 逻辑通过 RPC 调 Rust transport |
| 不迁移 | IMS SIP（sipgo）、gvisor netstack（swu-go）、DJI USB/QMI/MBIM、euicc-go LPA 全量重写 |
| 前置条件 | Rust 端必须先实现持久 PC/SC 会话（否则 Generic EMV 读卡器立即死亡） |
| BLE 位置 | 留在 Swift（CoreBluetooth 主进程约束），Rust 接管 channel 管理层 |
| 回退保障 | 保持 `go-compatibility` 模式，每次发布包含 Go-only 二进制 |

**与既有"务实路线"记忆的调和**：将"基于 Rust 重构"理解为**逐步将 Go 端功能迁移到 Rust 端，每步保持 Go 可回退**，而非一次性替换。这与用户风险偏好一致——已验证代码不被因架构理想化而重写，而是通过渐进迁移让 Rust 逐步承担更多职责。

---

## 1. 当前架构边界实勘

### 1.1 三语言职责现状

```
Swift (主进程)
  ├─ UI / 状态管理 (63 .swift 文件)
  ├─ CoreAudio HAL (模块通话音频直连)
  ├─ CoreBluetooth (BLE 读卡器 GATT — macOS 主进程约束)
  ├─ BLE Bridge (反向 IPC: 后端 → Swift BLE APDU 交换)
  └─ HTTP/JSON over Unix socket → proletariusd-rs

Rust (proletariusd-rs, gateway — Swift 的唯一后端入口)
  ├─ HTTP gateway + token 认证 (subtle::ConstantTimeEq)
  ├─ Go 子进程监督 (spawn/重启退避/可用性状态机)
  ├─ 路由归属判定 (ownership.rs route_owner: Rust vs Go)
  ├─ 读卡器只读: 枚举/AKA/身份 (proletarius-reader + proletarius-sim)
  ├─ 编解码: SMS / 传真 / 通话 / IMS / IKE
  ├─ 归档存储: 原子 JSON (flock + tmp+rename)
  └─ VoWiFi 代理策略 / 运营商解析

Go (proletariusd, Rust 的子进程)
  ├─ eSIM LPA: profile switch/download/rename/delete (euicc-go, 3789 行)
  ├─ 读卡器会话: Connect/Disconnect/RebuildSession/PowerCycleCard (reader_manager.go, 1227 行)
  ├─ 持久 PC/SC: ccidPersistentExchanger (reader_ccid_persistent.go, 103 行)
  ├─ BLE 通道: estkble.Channel (BLE bridge + CCID persistent 复用 Exchanger)
  ├─ VoWiFi: IKEv2/ESP (swu-go + gvisor) / IMS SIP (sipgo) / AMR
  └─ DJI 模块: USB/QMI/MBIM 硬件通信
```

### 1.2 Rust 已有的读卡器基础设施

| Crate | 能力 | 对应 Go 代码 |
|---|---|---|
| `proletarius-reader` (894 行) | PcscReaderScanner 枚举、authenticate_aka()、read_identity()、discover_usim_aid()、reader_auth_lock() | reader_manager.go 的 ListReaders / AuthenticateAKA / loadUSIMInfoLocked |
| `proletarius-sim` (750 行) | ApduTransport trait、exchange_apdu()（GET RESPONSE + 6Cxx）、authenticate_3g_apdu()、decode_iccid/imsi/msisp/smsp、AID 常量 | uicc-go 的 usim.NewReader + usim.New + decode 函数 |
| `proletarius-reader/session.rs` (137 行) | PcscSession (Shared 连接)、PcscTransaction (事务封装)、Drop 自动 disconnect(LeaveCard) | ccidPersistentExchanger 的 connect/transmit（但无持久会话） |
| `proletariusd/reader_auth.rs` | POST /api/v2/reader/authenticate (spawn_blocking 35s) | reader_manager.go AuthenticateAKA |
| `proletariusd/reader_identity.rs` | POST /api/v2/reader/identity (spawn_blocking 15s) | reader_manager.go loadUSIMInfoLocked |
| `proletariusd/reader_inventory.rs` | GET /api/reader/list (Rust PC/SC + Go BLE 合并) | reader_manager.go ListReaders |

### 1.3 Go 独占的模块（迁移目标）

| 模块 | 文件 | 行数 | 外部依赖 | Rust 等价 |
|---|---|---|---|---|
| 读卡器会话管理 | reader_manager.go | 1227 | goscard | ✅ pcsc crate |
| 持久 PC/SC 会话 | reader_ccid_persistent.go | 103 | uicc-go/ccid | ⚠️ 需新建 |
| eSIM Profile API | reader_esim_api.go | 620 | euicc-go | ❌ 无 Rust LPA |
| eSIM Manager (LPA) | esim/manager.go | 3789 | euicc-go | ❌ 无 Rust LPA |
| BLE channel/bridge | estkble/ (3 文件) | 207 | 纯 Go | ✅ 可移植 |
| VoWiFi 会话 | reader_vowifi_session.go | 883 | swu-go/sipgo | ❌ 不迁移 |
| VoWiFi 传输 | reader_vowifi_transport.go | 315 | swu-go/gvisor | ❌ 不迁移 |
| USIM 读取 (uicc-go) | reader_manager.go 内 | ~200 | uicc-go | ⚠️ 需补全 EF |

### 1.4 ownership.rs 当前状态

```
Rust 原生 (9 组件): gateway, reader_inventory, reader_identity, sms_codec, call_archive,
                     message_archive, fax_protocol, fax_session_manager, fax_archive
Go 阻塞 (5 组件):   dji_module, reader_session, reader_vowifi, message_runtime, device_snapshot
Swift (1 组件):     mac_audio
Rust 占比: 60%, ready_to_remove_go = false
```

---

## 2. 三审查交叉对比

### 2.1 共识矩阵

| 议题 | 审查一（架构迁移） | 审查二（技术设计） | 审查三（风险回归） | 共识度 |
|---|---|---|---|---|
| **迁移优先级** | 阶段 1: reader session 全部 | 持久会话 P0 + 并发 P0 + BLE P1 | 持久会话必须最先 + 只读 LPA 先行 | ★★★ |
| **LPA 策略** | 方案 B: Rust transport + Go LPA | 方案 B: Go euicc-go 调用 Rust APDU 端点 | 下载始终保留 Go 路径 | ★★★ |
| **VoWiFi** | 阶段 2 可迁移传输层 | 不建议（10-12 人月） | 不迁移 | ★★（一乐观，二三保守，采保守） |
| **P0 消解** | 编译期阻止 + 单 async 锁 | Send/Sync + 状态机 | 自然消解但需先修复 Go P0 再迁移 | ★★★ |
| **持久会话** | Rust pcsc Card owned handle | Mutex<PersistentCardInner> + revival | 必须先实现否则 EMV 卡死亡 | ★★★ |
| **BLE** | ReaderProtocol 枚举替代字符串 | ApduExchanger trait 统一传输 | 不迁移 BLE 层（Swift 约束） | ★★★ |
| **回退** | go-compatibility 模式 | per-operation 回退 | 保持 Go-only 二进制 + git tag | ★★★ |

### 2.2 差异与互补

- **审查一**偏宏观：给出完整路线图框架和 ownership 占比推演（60%→73%→80%）
- **审查二**偏技术细节：给出具体的 Rust 类型设计（SessionPhase 枚举、ApduExchanger trait、PersistentCardInner 结构）
- **审查三**偏风险管控：给出最危险操作排序（BPP 下载 > refresh=true > 持久会话）、APDU golden test 方案、测试卡锁卡恢复途径

三者无矛盾，互补形成完整画面：审查一提供"做什么"，审查二提供"怎么做"，审查三提供"怎么安全地做"。

---

## 3. 迁移范围与依赖分析

### 3.1 模块分类

**立即可迁移（Rust 已有基础或无复杂依赖）**

| 模块 | 依赖 | Rust 基础 | 迁移难度 |
|---|---|---|---|
| 持久 PC/SC 会话 | pcsc crate | PcscSession 已有，需包装为持久模式 | 小 |
| USIM 信息读取 (uicc-go 等价) | proletarius-sim | 已有 decode_iccid/imsi，需补全 EF-MSISDN/SPN/PNN/GID | 小 |
| ReconcilePresence | pcsc get_status_change | PcscReaderScanner 已有 | 小 |
| BLE channel 适配 | estkble 帧格式 → Rust | 可移植（纯协议层） | 小 |
| BLE bridge 客户端 | Unix socket JSON | Rust hyper/tokio | 小 |
| 能力判定 (ReaderProtocol) | 无 | 新建枚举 | 小 |

**中等努力（需要移植协议层或找等价库）**

| 模块 | 依赖 | Rust 等价 | 迁移难度 |
|---|---|---|---|
| eSIM APDU transport (MANAGE CHANNEL/SELECT/STORE DATA) | estkble.Channel | 需新建 Rust channel struct | 中 |
| EUICCInfo2 / ProfileInfoList 读取 | euicc-go (只读部分) | 需实现 BER-TLV + SGP.22 解码 | 中 |
| Profile 切换 (EnableProfile) | euicc-go ES10b | 需实现 ES10b APDU 构建 | 中 |
| Profile 删除/重命名 | euicc-go ES10d/ES10e | 同上 | 中 |

**暂不迁移（Go 生态根深蒂固或 macOS 平台限制）**

| 模块 | 原因 | 替代方案 |
|---|---|---|
| euicc-go LPA (3789 行) | 无 Rust 等价库，从头实现风险极高 | 方案 B: Rust transport + Go LPA via RPC |
| IMS SIP 运行时 (sipgo) | Rust 无生产级 SIP UAC 栈 | 保留 Go |
| gvisor netstack (VoWiFi) | Rust 无等价用户态网络栈 (smoltcp 不够) | 保留 Go |
| DJI USB/QMI/MBIM | Go 硬件 I/O 生态成熟 | 保留 Go |
| Profile 下载 (BPP 安装) | 最危险 APDU 序列，NV 存储损坏风险 | 始终保留 Go 路径 |

### 3.2 Go 依赖 → Rust 映射

| Go 依赖 | 用途 | Rust 等价 | 状态 |
|---|---|---|---|
| goscard | PC/SC 绑定 | `pcsc` crate v2.9 | ✅ 已有 |
| uicc-go | USIM 文件读取 + AKA | `proletarius-sim` | ⚠️ 需补全 EF |
| euicc-go | eUICC LPA (SGP.22) | 无 | ❌ 方案 B |
| estkble | BLE APDU 帧 + Channel | 可移植到 Rust | ✅ 可行 |
| swu-go | VoWiFi netstack (IKEv2/ESP) | `proletarius-ike` (编解码仅) | ❌ 缺状态机 + gvisor |
| sipgo | IMS SIP | 无 | ❌ 不迁移 |

---

## 4. Rust 设计方案：P0 问题如何消解

### 4.1 P0-1 / P0-3：锁序倒置与 RebuildSession 竞态

**Go 问题根因**：`rm.mu` ↔ `rm.authMu` 双锁嵌套获取顺序相反；RebuildSession "释放 authMu → 独占操作 → 重新获锁"窗口与 ReconcilePresence 竞态。

**Rust 设计**：

```
// 单一状态锁，无嵌套
struct ReaderSession {
    inner: tokio::sync::Mutex<SessionInner>,
}

struct SessionInner {
    phase: SessionPhase,         // Disconnected / Connecting / Connected / Rebuilding
    reader: Option<PcscReader>,  // 当前连接
    generation: u64,             // 身份变更递增号
    cached_usim: Option<UsimInfo>,
    protocol: ReaderProtocol,    // Estkme / GenericEmv / BleEstkme / BleLegacy
}

// RebuildSession 在同一把锁内完成，无释放窗口
async fn rebuild_session(&self, power_cycle: impl FnOnce() -> Result<()>) -> Result<()> {
    let mut inner = self.inner.lock().await;
    match inner.phase {
        SessionPhase::Connected => {
            inner.phase = SessionPhase::Rebuilding;  // CAS 转换
            // ReconcilePresence 见 Rebuilding 状态 → 跳过本轮回清
            drop(inner);  // 临时释放做 power cycle
            power_cycle().await?;
            let mut inner = self.inner.lock().await;  // 重新获锁
            if inner.phase != SessionPhase::Rebuilding {
                // 期间状态被改写（读卡器拔出等）
                return Err(ReaderGoneDuringRebuild);
            }
            inner.phase = SessionPhase::Connected;
        }
        _ => return Err(InvalidPhase),
    }
    Ok(())
}
```

**消解原理**：
- Rust `std::sync::MutexGuard` 是 `!Send`——编译期阻止跨 `.await` 持有 std 锁
- `tokio::sync::Mutex` 的 `lock().await` 是 async-safe，且同一把锁不存在嵌套获取顺序问题
- `SessionPhase` 枚举 + CAS 转换：ReconcilePresence 检查 phase == Rebuilding 则跳过，rebuild 重新获锁后检查 phase 未被篡改
- 不存在第二个锁（无 authMu），AB-BA 死锁在结构上不可能

### 4.2 P0-2：BLE 读卡器能力判定

**Go 问题根因**：`isESTKmeReader("BLE:UUID")` 恒 false（UUID 不含 "estkme" 字样）。

**Rust 设计**：

```
#[derive(Clone, Copy, PartialEq)]
enum ReaderProtocol {
    EstkmeUsb,      // ESTKme-RED USB-C CCID
    GenericEmv,     // Generic EMV CCID（固件缺陷，跳过电源操作）
    BleEstkme,      // ESTKme-RED BLE（支持 Reset）
    BleLegacy,      // 9eSIM / SimLink（legacy 3 字节帧）
    BleBeeSim,      // BeeSIM 协议（AE30 服务）
}

struct ReaderCapability {
    protocol: ReaderProtocol,
    software_reset: bool,       // 是否支持 PC/SC 软复位
    power_cycle: PowerCycleMode, // None / UnpowerCard / BleReset
    persistent_session: bool,   // 是否使用持久会话
    refresh_on_switch: bool,    // 是否允许 refresh=true
}

impl ReaderCapability {
    fn from_ble_bridge(status: &BleBridgeStatus) -> Self {
        // 连接时通过 BLE bridge status 响应中的设备名/服务 UUID 一次性确定
        match status.device_name.as_str() {
            n if n.contains("ESTKme") => ReaderCapability {
                protocol: ReaderProtocol::BleEstkme,
                software_reset: true,
                power_cycle: PowerCycleMode::BleReset,
                persistent_session: false,
                refresh_on_switch: false,  // BLE 不触发 refresh
            },
            // ...
        }
    }
    fn from_pcsc_name(name: &str) -> Self {
        match name {
            n if n.contains("estkme") => /* EstkmeUsb */,
            _ => /* GenericEmv, 跳过电源操作 */,
        }
    }
}
```

**消解原理**：能力判定在**连接时一次性确定**并存储为 `ReaderCapability`，后续所有操作（PowerCycleCard / RebuildSession / profile switch）从该结构派生行为，不再依赖运行时字符串匹配。

### 4.3 持久 PC/SC 会话（Rust 新建，前置条件）

```
struct PersistentCard {
    inner: std::sync::Mutex<PersistentCardInner>,
}

struct PersistentCardInner {
    card: Option<pcsc::Card>,     // 持久打开的 Card handle
    reader_name: String,
    last_revive: Option<Instant>,  // revival 冷却时间戳
    protocol: ReaderProtocol,
}

impl PersistentCard {
    fn transmit(&self, apdu: &[u8]) -> Result<Vec<u8>> {
        let mut inner = self.inner.lock().unwrap();
        if inner.card.is_none() {
            self.open_locked(&mut inner)?;
        }
        match inner.card.as_ref().unwrap().transmit(apdu) {
            Ok(resp) => Ok(resp),
            Err(_) => {
                // Transmit 错误 → 关闭并重建
                inner.card = None;
                if self.should_revive(&inner) {
                    self.open_locked(&mut inner)?;
                    inner.card.as_ref().unwrap().transmit(apdu)
                } else {
                    Err(RevivalCooldown)
                }
            }
        }
    }
}
```

**与 Go 的关键差异**：
- Rust `pcsc::Card` 是 owned handle，无 goscard 的 `Initialize/Finalize` 全局副作用
- `Context` 可池化复用，无全局状态
- Generic EMV 保护：`protocol == GenericEmv` 时跳过所有电源操作（reset/unpower），仅 LeaveCard

---

## 5. eSIM LPA 分层方案（方案 B）

### 5.1 架构

```
Rust 端 (transport owner)                  Go 端 (LPA logic)
┌──────────────────────────┐              ┌────────────────────────┐
│ PersistentCard           │              │ esim.Manager            │
│  ├─ BLE bridge client    │◄──APDU──────│  ├─ lpa.Client          │
│  ├─ PC/SC persistent     │   over       │  │   ├─ ES10b (profile) │
│  └─ ReaderCapability     │   Unix sock  │  │   ├─ ES10c (download)│
│                          │   or FFI     │  │   └─ ES10d (delete)  │
│ POST /api/v2/apdu/transmit│              │  └─ APDU coordinator   │
│ POST /api/v2/apdu/manage- │              │                         │
│       channel             │              │ POST /api/reader/esim/* │
└──────────────────────────┘              └────────────────────────┘
```

### 5.2 接口设计

Rust 端暴露的 APDU transport API（通过 Unix socket HTTP/JSON 或直接 FFI）：

| 端点 | 功能 | 对应 Go 当前代码 |
|---|---|---|
| `POST /api/v2/apdu/transmit` | 在持久会话上发送裸 APDU | estkble.Channel.Transmit / ccidPersistentExchanger.Exchange |
| `POST /api/v2/apdu/open-channel` | MANAGE CHANNEL + SELECT AID | estkble.Channel.OpenLogicalChannel |
| `POST /api/v2/apdu/close-channel` | MANAGE CHANNEL CLOSE | estkble.Channel.CloseLogicalChannel |
| `POST /api/v2/apdu/power-cycle` | 卡片复位（按 ReaderCapability 分派） | reader_manager.go PowerCycleCard |

Go 端 `euicc-go` 的 `driver.SmartCardChannel` 接口改为调用 Rust 端点（而非直接 goscard）。

### 5.3 迁移路径

1. Rust 端实现 APDU transport API + 持久会话 + channel 管理
2. Go 端 `newReaderChannel` 改为创建 "Rust transport adapter"（实现 `driver.SmartCardChannel`，内部 HTTP RPC 调 Rust）
3. Go 端 `euicc-go` LPA 逻辑不变，只是底层传输从 goscard → Rust APDU 端点
4. 验证：Go LPA 通过 Rust transport 的 APDU 序列与原 Go 直连 goscard 的序列 byte-for-byte 一致

**关键收益**：Rust 拥有 PC/SC 会话控制权，Go 的 P0 锁问题（mu/authMu/RebuildSession）不再存在——Rust 端的 SessionInner 单锁管理一切，Go 端只剩无状态 LPA 逻辑。

---

## 6. 分阶段迁移路线图

### 阶段 0：前置准备（1 周，不迁移任何功能）

| 任务 | 产出 |
|---|---|
| Rust 端实现 `PersistentCard` + `ReaderCapability` | 新 crate `proletarius-reader-session` |
| Rust 端补全 `proletarius-sim`：EF-MSISDN / EF-SPN / EF-PNN / GID | 扩展 proletarius-sim crate |
| Go 端增加 APDU 日志录制（环境变量控制） | golden test 数据 |
| Git tag `go-verified-2026-09-18` | 回退锚点 |

**退出条件**：Rust 单元测试（PersistentCard + ScriptedTransport）通过；Go APDU 录制覆盖三卡 profile 列表 + 切换序列。

### 阶段 1：读卡器会话迁移（2-3 周）

| 任务 | ownership 变化 |
|---|---|
| Rust 实现 `ReaderSession`（Connect/Disconnect/RebuildSession/ReconcilePresence） | reader_session: go/shadow → rust/native |
| Rust 实现 BLE bridge 客户端（替代 Go estkble.Client） | — |
| Rust 实现 APDU transport API（transmit/open-channel/close-channel/power-cycle） | — |
| Go `newReaderChannel` 改为 Rust transport adapter | — |
| Rust 端 USIM 读取补全（替代 Go uicc-go 调用链） | reader_identity: 已 rust → 全 rust |

**P0 消解验证**：
- T3 并发压测（AKA + profile 操作并发 10 分钟）——Rust 单锁无死锁
- T5 RebuildSession 竞态——SessionPhase 状态机阻止旧值覆盖
- T6 能力判定矩阵——ReaderCapability 枚举覆盖四种读卡器

**退出条件**：三卡三读卡器全功能正常 + 三卡 IMS 注册不回归 + Generic EMV 卡不死亡。

**回退条件**：任何读卡器出现异常 → `go-compatibility` 模式回退。

### 阶段 2：只读 LPA 迁移（1-2 周）

| 任务 | 说明 |
|---|---|
| Rust 实现 EUICCInfo2 读取 | BER-TLV 解码 + SGP.22 字段提取 |
| Rust 实现 ProfileInfoList 读取 | ES10b GetProfilesInfo |
| Rust 实现 eUICCChipInfo 读取 | eUICC 版本/SGP.22 版本/EID |
| `ownership.rs` 更新路由 | eSIM 只读端点从 Go 转到 Rust |

**验证**：Rust 只读 LPA 结果与 Go golden test 100% 一致。

### 阶段 3：Profile 切换迁移（2-3 周，谨慎评估后决定）

| 任务 | 风险 |
|---|---|
| Rust 实现 ES10b ProfileEnable APDU 构建 | 中（写操作，但可回退 Go） |
| Rust 实现 refresh=true 条件控制 | 高（仅在 EstkmeUsb 上允许） |
| Rust 实现 PowerCycleCard 全分支 | 中（按 ReaderCapability 分派） |

**验证**：三卡切换 + ICCID 变更 + IMS 注册恢复 + UI 不闪断。

**回退**：per-operation 回退——Rust 切换失败自动转发 Go 路径。

### 阶段 4（暂不启动）：Profile 下载 / VoWiFi / IMS

- Profile 下载：BPP 安装最危险，始终保留 Go 路径，直到 Rust LPA 通过废弃卡验证
- VoWiFi netstack / IMS SIP：10-12 人月，收益不抵风险，保持 Go
- 此阶段启动前需单独确认

---

## 7. 风险控制

### 7.1 最危险操作排序

| Tier | 操作 | 后果 | 缓解 |
|---|---|---|---|
| **1** | Rust 实现 BPP 下载 | eUICC NV 存储损坏，需 SM-DP+ 重新下载 | **阶段 4，暂不启动**；始终保留 Go |
| **2** | Rust 缺持久会话 Generic EMV 死亡 | 物理拔插恢复 | **阶段 0 必须先实现** |
| **3** | refresh=true 条件遗漏 | BLE/EMV 卡无响应 | 单元测试 + ReaderCapability 枚举 |
| **4** | Rust/Go 双运行 PC/SC 冲突 | APDU 交错，卡状态不确定 | 阶段 1 后 Rust 独占 PC/SC 会话 |

### 7.2 跨进程 PC/SC 互斥

迁移期间 Rust 和 Go 是不同 Unix 进程，PC/SC Transaction 是跨进程的（SCardBeginTransaction 阻止其他 handle 的 Transmit）。

**阶段 1 前**：Rust 端 PC/SC 操作（AKA/identity）和 Go 端 LPA 操作可能同时访问同一张卡。
- 当前缓解：Go 检测 `PROLETARIUS_CARRIER_SOCKET` 优先委托 Rust 做 AKA
- 残留风险：eSIM LPA 操作无委托机制

**阶段 1 后**：Rust 独占 PC/SC 会话，Go 通过 Rust transport API 访问卡，不存在双进程直接操作 PC/SC。

### 7.3 回退保障

| 层级 | 机制 | 触发条件 |
|---|---|---|
| L1 | per-operation 回退 | Rust 操作失败 → 自动转发 Go |
| L2 | runtime feature flag | `PROLETARIUS_RUST_READER=0` 禁用 Rust reader，全走 Go |
| L3 | go-compatibility 模式 | `ProletariusBackendMode=go-compatibility` 启动 Go-only 二进制 |
| L4 | git tag 回退 | `go-verified-2026-09-18` 恢复已验证版本 |

### 7.4 测试卡保护

| 卡 | 锁卡恢复途径 | 保护策略 |
|---|---|---|
| VDF eSIM | ESTKme-RED USB-C 物理拔插 | 阶段 3 前不迁移切换 |
| CTExcel eSIM | 同上；NV 损坏需 SM-DP+ 重下 | 下载始终走 Go |
| Giffgaff eSIM | 同上 | 同上 |
| 9eSIM BLE | 长按电源键或插充电线 | BLE 层不迁移 |

---

## 8. 测试策略

### 8.1 APDU Golden Test

1. **录制**：Go 端 `estkble.Channel.Transmit` 和 `ccidPersistentExchanger.Exchange` 增加环境变量控制的 APDU 日志
2. **格式**：`{command: [bytes], response: [bytes], stage: "open_channel|select_aid|enable_profile|..."}`
3. **回放**：Rust `ScriptedTransport` 加载 golden 数据，验证生成的 APDU 序列一致
4. **覆盖**：VDF/CTExcel/Giffgaff 三卡 × profile 列表 + 切换序列

### 8.2 Rust 单元测试扩展

```
proletarius-reader-session/tests/
├── test_persistent_card_recovery       // 模拟卡无响应后重建
├── test_reader_capability_matrix       // 四种 ReaderProtocol 能力判定
├── test_rebuild_session_phase_guard    // RebuildSession 竞态防护
├── test_power_cycle_dispatch           // PowerCycleCard 按 protocol 分派
├── test_refresh_true_only_estkme_usb   // refresh 条件控制
├── test_aid_scan_order                 // AID 扫描顺序
├── test_golden_profile_list            // 回放 Go APDU 序列
└── test_golden_profile_switch          // 回放 Go APDU 序列
```

### 8.3 真机回归矩阵

| 测试 | 阶段 0 | 阶段 1 | 阶段 2 | 阶段 3 |
|---|---|---|---|---|
| ESTKme USB profile 列表 | Go | Rust transport + Go LPA | Rust 只读 LPA | Rust 全 LPA |
| ESTKme USB profile 切换 | Go | Go LPA via Rust transport | Go LPA | Rust LPA |
| Generic EMV profile 列表 | Go | **Rust 持久会话** | Rust | Rust |
| 9eSIM BLE 连接 | Go | Rust BLE client | Rust | Rust |
| 三卡 IMS 注册 | Go VoWiFi | Go VoWiFi（不变） | Go VoWiFi | Go VoWiFi |
| VoWiFi 通话/传真 | Go | Go（不变） | Go（不变） | Go（不变） |

---

## 9. 参考项目启示

### 9.1 NekokoLPA2 的 Rust 层

NekokoLPA2 是 Flutter(Dart) + Rust（flutter_rust_bridge）架构——Rust 承担 modem 通信（qrtr），Dart 承担读卡器适配。这与 Proletarius 的多语言分工同构。

其 Rust 层（`rust/src/`）只有 8 个文件，职责清晰：API 模块 + qrtr 总线 + modem 操作。**启示**：Rust 层不需要臃肿——保持 crate 职责单一，通过清晰接口暴露能力。

### 9.2 ios-carrier-profiles（不变）

运营商画像 selector+bundle 机制与 Rust 重构正交——无论读卡器层在 Rust 还是 Go，carrier profile 的数据化与外置化路径不受影响。阶段 1-3 完成后可独立推进。

---

## 10. 结论

- **可行**：Rust 已有 pcsc + APDU + AKA 基础设施，读卡器会话层迁移是自然的架构演进
- **有益**：三个 P0 问题在 Rust 的所有权模型 + 单 async 锁 + 编译期线程安全中自然消解，无需打补丁
- **可控**：分 4 阶段渐进迁移，每阶段有明确退出/回退条件，保持 Go-only 可回退
- **有界**：LPA 写操作（尤其 BPP 下载）和 VoWiFi/IMS 暂不迁移——方案 B（Rust transport + Go LPA）兼顾架构收益与风险控制
- **前置**：持久 PC/SC 会话必须在阶段 0 先实现（否则 Generic EMV 读卡器立即死亡）

**建议**：从阶段 0（前置准备 + APDU 录制 + git tag）开始，1 周内完成。阶段 0 的产物（PersistentCard、ReaderCapability、golden test 数据）是后续所有阶段的基础，且不改变任何现有行为。

---

*审议任务索引：架构迁移 ses_f4c772d53ffdI7pqrK7D18tG8L · 技术设计 ses_f4c772d54ffeUiVQ0lCQ4bELA0 · 风险回归 ses_f4c772d53ffeqjNLT4OPk7UKwl*
*参考项目：github.com/iebb/NekokoLPA2（Rust 层架构参考） · github.com/iniwex5/ios-carrier-profiles*
*本方案为建议性质，未修改任何代码；阶段 0 实施前请确认。*