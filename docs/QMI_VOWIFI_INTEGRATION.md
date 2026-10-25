# QMI 支持与 VoWiFi 集成说明

> 本文档记录 Proletarius 新增 **QMI 支持** 与 **VoWiFi（WiFi 通话）** 两个功能模块的设计、改动清单与验证步骤。
> 适用范围：Proletarius macOS 原生应用。

## 1. 背景与目标

Proletarius 原有的硬件链路为 **AT 指令主链路**（串口或 USB bulk AT 桥），不包含 QMI 通道。
本改动在**不破坏现有 AT 功能**的前提下，增加：

1. **QMI 支持**：完整接入 quectel-qmi-go 后端，提供 IMS/IMSA 状态查询、运营商选择、SIM 鉴权等 QMI 能力。
2. **VoWiFi 落地**：**AT 指令为主**（`AT+QCFG="ims"` / `AT+QCFG="wfc"`），**QMI 查询为辅**（IMS service / IMSA service 状态校验），不依赖 QMI 底层通道可用性。

## 2. 架构决策

```
Proletarius（macOS）
├── AT 主链路（原有，不变）
│   ├── 串口 /dev/tty.usbmodem*（发现失败时回退 USB bulk AT）
│   └── 短信 / eSIM / 通话 / 拨号 等一切现有功能
└── QMI 增强通道（新增，可选）
    ├── -backend=qmi 时启用（默认 at）
    ├── quectel-qmi-go manager core（NoDial + DataPlanePolicyDisabled）
    ├── libusb 传输层（darwin 特有，替代 Linux /dev/cdc-wdmX）
    └── 提供 VoWiFi API、IMS/IMSA 状态查询等增强能力
```

要点：

- **QMI 是可选增强**：`-backend=qmi` 时在 AT 主链路之外叠加 QMI 通道；QMI 不可用时 AT 一切功能照常。
- **VoWiFi 以 AT 为主**：`QueryVoWiFiStatus` / `ApplyVoWiFi` 先走 AT，QMI 结果仅作为辅助诊断字段合并（`qmi_error` 等）。
- **darwin 传输层**：Proletarius 随附的本地 QMI 适配层已注入 macOS libusb 传输实现。

## 3. 改动清单

### 3.1 vendored 库与传输层

| 文件 | 说明 |
| --- | --- |
| `backend/third_party/quectel-qmi-go/` | Proletarius 随附的本地 QMI 适配层 |
| `backend/third_party/quectel-qmi-go/pkg/qmi/transport_libusb_darwin.go` | **新增**：`darwin && cgo` libusb 传输层；扫描全部 class-0xFF bulk 接口并用 `CTL_SYNC` 探测确认 QMI 接口；实现 `Read/Write/Close/SetReadDeadline`（读写分锁） |
| `backend/third_party/quectel-qmi-go/pkg/qmi/transport_libusb_stub.go` | **新增**：`darwin && !cgo` stub，返回明确报错（提示需 CGO_ENABLED=1 + libusb） |

### 3.2 后端内部

| 文件 | 说明 |
| --- | --- |
| `backend/internal/backend/qmi_source_adapter.go` | **新增**：`qmSourceAdapter` 包装 `*manager.Manager`，实现 `QMISource` 全部 32 个方法及 NAS 控制、SIM 鉴权等可选接口；编译期断言保证接口完整性 |
| `backend/internal/backend/vowifi.go` | **新增**：VoWiFi 状态模型、`ParseVoWiFiATStatus`、`ApplyVoWiFi` / `QueryVoWiFiStatus`、AT 命令编译、QMI 合并 |
| `backend/go.mod` | 将 QMI 依赖固定到随源码保存的本地适配层 |

### 3.3 主程序接线

| 文件 | 说明 |
| --- | --- |
| `backend/cmd/proletarius-macos/qmi_enhance.go` | **新增**：`qmiEnhancement`（manager 封装）、`startQMIEnhancement`、QMI 探测回调、VoWiFi HTTP 处理器 |
| `backend/cmd/proletarius-macos/main.go` | `-backend` flag（at\|qmi）、`app.qmiEnh` 字段、路由 `GET/POST /api/vowifi` |

## 4. 构建依赖与编译

macOS 上编译需要：

```bash
# 1. 安装 libusb（Homebrew）
brew install libusb

# 2. 编译（必须启用 cgo）
cd backend
CGO_ENABLED=1 go build ./...
```

- 不启用 cgo 时，`transport_libusb_stub.go` 使 darwin 构建仍可编译，但 QMI 传输层返回明确报错。
- 本机（无 Go）无法编译，以上步骤需在有 Go + libusb 的机器上执行。

## 5. 运行与验证步骤

### 5.1 运行

```bash
# 默认（AT 主链路，无 QMI）
./proletariusd

# 启用 QMI 增强（AT 主链路 + QMI 叠加）
./proletariusd -backend=qmi
```

### 5.2 API 验证

```bash
# 查询 VoWiFi 状态
curl --unix-socket /tmp/proletarius.sock http://localhost/api/vowifi

# 设置 VoWiFi（WiFi 优先）
curl --unix-socket /tmp/proletarius.sock \
  -X POST http://localhost/api/vowifi \
  -H 'Content-Type: application/json' \
  -d '{"ims_wifi_calling_enabled": true, "wifi_calling_mode": 2}'
```

预期返回（AT + QMI 合并）：

```json
{
  "ims_wifi_calling_enabled": true,
  "ims_call_mode_set": true,
  "wifi_calling_mode": 2,
  "qmi_enabled": true,
  "qmi_wifi_first": true,
  "qmi_in_use": true,
  "qmi_wifi_registered": true
}
```

若 QMI 通道不可用，`qmi_*` 字段不出现（或 `qmi_error` 携带诊断），AT 部分仍返回。

### 5.3 验证编译

```bash
cd backend
go vet ./...
go build ./...
```

## 6. 注意事项

1. **libusb 接口探测**：QMI 接口在模块上较靠后的 class-0xFF 接口，传输层已扫描全部 class-0xFF 接口（不限 interface 2/3），并用 `CTL_SYNC` 探测确认。
2. **`CTL_SYNC` 探测帧**：与 `Packet.Marshal` 生成的 CTL_SYNC 请求逐字节一致（QMUX 头 + CTL 头，Length=0x0B，MessageID=0x0027）。
3. **数据面策略**：`DataPlanePolicyDisabled` + `NoDial=true`（DJI 蜂窝数据走 macOS USB-NET，QMI 不需要拨号）。
4. **前端界面**：VoWiFi 设置 UI（`app/Sources/Views/`）尚未实现，仅后端 API 可用（可选后续）。
5. **升级注意**：`quectel-qmi-go` 的 vendor 目录内含测试文件，`go build ./...` 不会编译测试，无需特别处理。
