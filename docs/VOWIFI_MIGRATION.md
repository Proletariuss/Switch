---
AIGC:
  ContentProducer: '001191110102MAD55U9H0F10002'
  ContentPropagator: '001191110102MAD55U9H0F10002'
  Label: '1'
  ProduceID: 'e8593d60-07e7-4748-bf8a-178d04160114'
  PropagateID: 'e8593d60-07e7-4748-bf8a-178d04160114'
  ReservedCode1: 'ea87a10e-32f0-45d4-a418-f72748b713c8'
  ReservedCode2: 'ea87a10e-32f0-45d4-a418-f72748b713c8'
---

# VoWiFi 功能迁移文档

## 一、背景

本项目 Proletarius 原有两套 VoWiFi 实现，均已移除，等待重新构建：

1. **纯 Go IKEv2/IPsec 路径**（已删除）：通过 PC/SC CCID 读卡器获取 SIM 鉴权素材，使用内置纯 Go IKEv2/IPsec 协议栈连接 ePDG，建立隧道后向 P-CSCF 发起 IMS/SIP 注册。代码位于 `backend/internal/vowifi/`（ike/、ims/ 等子包）和 `backend/cmd/proletarius-macos/reader_vowifi_api.go`、`reader_vowifi_aka_adapter.go`。

2. **模块 AT+CFUN 路径**（已删除）：通过 Quectel 模块的 AT 命令 + QMI 实现 VoWiFi，模块内置协议栈处理 IKEv2/IPsec/IMS。代码位于 `backend/cmd/proletarius-macos/vowifi_flow.go` 和 `backend/internal/backend/vowifi.go`。

## 二、已删除的文件

| 文件/目录 | 说明 |
|---|---|
| `backend/internal/vowifi/` （整个目录） | 纯 Go IKEv2/IPsec + IMS/SIP 协议栈（37+ 文件） |
| `backend/cmd/proletarius-macos/reader_vowifi_aka_adapter.go` | CCID 读卡器到 EAP-AKA 的适配器 |
| `backend/cmd/proletarius-macos/vowifi_flow.go` | 模块 AT+CFUN 模式 VoWiFi 异步编排 |
| `backend/internal/backend/vowifi.go` | 模块 VoWiFi 公共逻辑层（AT 命令编译/QMI 合并/状态查询） |

## 三、保留的文件（桩函数 + 契约）

### `backend/cmd/proletarius-macos/reader_vowifi_api.go`（桩函数）

保留了完整的 HTTP API 契约和数据结构，功能部分为桩：

- **HTTP 路由**：
  - `GET /api/reader/vowifi` — 返回 VoWiFi 状态（桩：始终返回 disconnected）
  - `POST /api/reader/vowifi` — 启用/禁用 VoWiFi（桩：启用返回"尚未实现"，禁用重置状态）
  - `GET /api/reader/imei` — 获取 IMEI
  - `PUT /api/reader/imei` — 设置 IMEI
  - `DELETE /api/reader/imei` — 清除 IMEI

- **数据结构**：`readerVoWiFiStatus`、`readerVoWiFiRequest`、`tunnelState`、`imsState` 等

- **IMEI 管理**：完整的持久化逻辑（加载/保存到 `~/Library/Application Support/Proletarius/imei.json`）

- **`ConstructEPDGFQDN(mcc, mnc)` 函数**：从 MCC/MNC 构造 ePDG FQDN（`epdg.epc.mnc{MNC}.mcc{MCC}.pub.3gppnetwork.org`，MNC 补零至 3 位）

### `backend/internal/backend/qmi_types.go`（类型定义）

从已删除的 `vowifi.go` 中提取了被非 VoWiFi 代码引用的类型：

- `QMIProbeFunc` — QMI 辅助查询回调
- `QMISetFunc` — QMI IMS 设置回调
- `QMIOperatorFunc` — QMI 运营商信息查询回调
- `VoWiFiATProvider` — AT 命令执行能力接口

### 已移除的 HTTP 路由

| 路由 | 原始 handler |
|---|---|
| `GET /api/vowifi` | `a.getVoWiFi` — 模块 VoWiFi 状态查询 |
| `POST /api/vowifi` | `a.setVoWiFi` — 模块 VoWiFi 启用/禁用 |
| `GET /api/vowifi/result` | `a.getVoWiFiResult` — 异步操作结果 |

### `main.go` 中的变更

- 移除了 `app` 结构体中的 `voWiFiMu`、`voWiFiOperation`、`voWiFiResult`、`voWiFiEPDGAutoDetect` 字段
- 移除了 `/api/vowifi` 和 `/api/vowifi/result` 路由注册
- 移除了 `voWiFiEPDGAutoDetect: true` 初始化

### `qmi_enhance.go` 中的变更

- 移除了 `getVoWiFi`、`setVoWiFi`、`getVoWiFiResult` 三个 HTTP handler
- 保留了 `qmiEnhancement` 结构体和 QMI 探测回调（`qmiProbe`、`qmiSetIMS`、`qmiOperator`）供将来使用
- 保留了 `RunCommand` 方法（实现 `backend.VoWiFiATProvider` 接口）

### `reader_manager.go` 中的变更

- 移除了 `ConstructEPDGFQDN` 委托函数（已内联到 `reader_vowifi_api.go`）
- 移除了 `backend` 包 import

## 四、前端 API 契约（Swift 端）

前端（SwiftUI）通过 `APIClient.swift` 调用以下 VoWiFi 相关 API：

```
GET    /api/reader/vowifi      → ReaderVoWiFiStatus
POST   /api/reader/vowifi      → { enable: Bool }
GET    /api/reader/imei        → { imei: String, config: Bool }
PUT    /api/reader/imei        → { imei: String }
DELETE /api/reader/imei        → { message: String }
```

### ReaderVoWiFiStatus JSON 结构

以下身份号码均为合成示例，仅用于说明字段格式，不对应真实订阅或设备。

```json
{
  "available": bool,
  "mcc": "234",
  "mnc": "33",
  "imsi": "001010123456789",
  "epdg_fqdn": "epdg.epc.mnc033.mcc234.pub.3gppnetwork.org",
  "imei": "490154203237518",
  "tunnel_state": "disconnected|connecting|connected|error",
  "ims_state": "idle|registering|registered|failed",
  "message": "状态描述",
  "local_ipv4": "",
  "local_ipv6": "2a00:23ee:...",
  "pcscf": ["2a01:4c8:f000:f53::1"],
  "ike_encryption": "aes-cbc-128",
  "esp_encryption": "aes-cbc-128",
  "responder_auth": "verified",
  "registered_msisdn": "+447...",
  "ims_transport": "udp|tcp",
  "ims_security_mode": "optional",
  "sms_ready": bool
}
```

## 五、重新构建 VoWiFi 需要实现的核心接口

原始 `vowifi` 包对外暴露的接口契约（供重新实现参考）：

### 核心接口

```go
// AKAProvider — EAP-AKA 认证源（读卡器实现）
type AKAProvider interface {
    CheckReady(ctx context.Context, identity SIMIdentity) (AKAEvidence, error)
    Authenticate(ctx context.Context, identity SIMIdentity, challenge AKAChallenge) (AKAResult, error)
}

// TunnelProvider — IKEv2/IPsec 隧道
type TunnelProvider interface {
    Start(ctx context.Context, req TunnelRequest) (TunnelSession, error)
}

// TunnelSession — 隧道会话
type TunnelSession interface {
    Evidence() TunnelEvidence
    Close(ctx context.Context) error
}

// IMSProvider — IMS/SIP 注册
type IMSProvider interface {
    Start(ctx context.Context, req IMSRequest) (IMSSession, error)
}

// IMSSession — IMS 会话
type IMSSession interface {
    Evidence() IMSEvidence
    EnableSMS(ctx context.Context) (SMSEvidence, error)
    Close(ctx context.Context) error
}

// SMSSender — SMS over IMS
type SMSSender interface {
    SendSMS(ctx context.Context, req SMSSubmitRequest) (SMSSubmitResult, error)
}

// CallController — IMS 通话
type CallController interface {
    DialCall(ctx context.Context, req CallDialRequest) (*Call, error)
    AnswerCall(ctx context.Context, callID string) error
    HangupCall(ctx context.Context, callID string) error
}
```

### 核心数据结构

```go
type SIMIdentity struct {
    ICCID, IMSI, HomeMCC, HomeMNC, EPDG, SMSC, IMEI string
}

type TunnelEvidence struct {
    Established   bool
    Name          string  // utun 接口名（macOS）
    DataplaneMode string  // "userspace"
    LocalIPv4     string
    LocalIPv6     string
    PCSCF         []string
    IKEEncryption string
    ESPEncryption string
    ResponderAUTH string
}

type IMSEvidence struct {
    Registered       bool
    AssociatedMSISDN string
    Transport        string
    SecurityMode     string
}
```

### HTTP API 挂载点

在 `main.go` 的 `routes()` 函数中，以下路由已注册到桩函数：

```go
mux.HandleFunc("GET /api/reader/vowifi", a.readerVoWiFi)
mux.HandleFunc("POST /api/reader/vowifi", a.readerVoWiFi)
mux.HandleFunc("GET /api/reader/imei", a.readerIMEIHandler)
mux.HandleFunc("PUT /api/reader/imei", a.readerIMEIHandler)
mux.HandleFunc("DELETE /api/reader/imei", a.readerIMEIHandler)
```

## 六、项目技术栈

- **后端**：Go 1.26，模块 `proletarius.local/core`
- **前端**：macOS SwiftUI，XcodeGen（`project.yml`），Liquid Glass 设计
- **构建**：Xcode Build Phase 编译 Go 后端 → 嵌入 app bundle
- **后端入口**：`backend/cmd/proletarius-macos/main.go`
- **进程通信**：Unix Domain Socket（`/tmp/proletarius.sock`）
- **SIM 读卡器**：PC/SC CCID（ESTKme 等），通过 `github.com/damonto/uicc-go` 和 `github.com/damonto/euicc-go`
- **QMI**：vendored `github.com/iniwex5/quectel-qmi-go`，含 macOS libusb 传输
- **测试设备**：ESTKme RED 读卡器 + CTExcel UK SIM 卡（MCC=234, MNC=33, IMSI=001010123456789）
- **代理环境**：Shadowrocket（utun8 TUN），英国出口线路

## 七、历史调试经验（避免踩坑）

1. **IKE 隧道建立可成功**（EAP-AKA verified），但 SIP REGISTER 到 P-CSCF 失败
2. **macOS utun 接口的路由问题**：Shadowrocket 的 utun8 TUN 接口接管了系统路由表的 default 路由，导致 SIP UDP 流量可能被截获
3. **`IPV6_BOUND_IF`/`IP_BOUND_IF`**：macOS 上需要用 socket option 绑定到 utun 接口而非绑定 IP 地址，否则 `net.Dialer` 的源地址选择与路由接口不一致会导致 `broken pipe` / `network is unreachable`
4. **`tunnelName` 函数**：旧实现的自定义接口名前缀仅用于 Linux；macOS 上 utun 接口名由内核分配（如 `utun10`），需要从 `ChildSAConfig.Name` 回传到 `TunnelEvidence.Name`
5. **ePDG FQDN 构造**：MNC 必须补零至 3 位（`33` → `033`）
6. **MD5 Digest 认证**：CTExcel UK（Orange UK）走 MD5 空密码路径，ISIM 静态身份可能为空

## 八、实现说明

运营商参数、配置解析和 QMI 适配均以 Proletarius 内置实现及随附的第三方许可清单为准；产品界面和运行日志不展示历史参考项目标记。
