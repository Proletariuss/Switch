---
AIGC:
  ContentProducer: '001191110102MAD55U9H0F10002'
  ContentPropagator: '001191110102MAD55U9H0F10002'
  Label: '1'
  ProduceID: 'ebe2dbc5-bb6d-41e3-9eb9-f55885fbd83b'
  PropagateID: 'ebe2dbc5-bb6d-41e3-9eb9-f55885fbd83b'
  ReservedCode1: '41419ae4-8a15-4e8f-b623-746bbd8a4baa'
  ReservedCode2: '41419ae4-8a15-4e8f-b623-746bbd8a4baa'
---

# VoWiFi 启动失败排查报告 · 第二阶段（8.31 配置核查）

- **日期**：2026-09-19 深夜
- **背景**：用户已切换至 Surge 8.31 配置（含 VoWiFi 分流规则），要求核查并给出修复方案；明确表示不想开增强模式，只想要配置模式
- **本报告取代第一阶段报告中的修复建议**（第一阶段的"加规则"已存在于 8.31 配置中，但仍失败——原因在节点，不在规则）

---

## 0. 核查结论（三个被实验证实的事实）

### 事实一：增强模式实际处于开启状态（与你的认知相反）

你说没有开启增强模式，但系统证据一致表明它在运行：

| 证据 | 数值 |
|---|---|
| Surge 网络系统扩展 | `com.nssurge.surge-mac.ne` **[activated enabled]**，进程存活 |
| TUN 接口 | `utun8` UP，地址 **10.254.1.1**（Surge 增强模式标志地址） |
| 路由劫持 | `1/8、2/7、4/6、8/5 → 10.254.1.1 → utun8` 的多段拆分路由（Surge 增强模式标志路由） |
| ePDG 路由归属 | `route -n get 31.94.76.9` → interface: **utun8** |
| 实时连接 | Surge 活动请求的 interface 字段为 utun8（22:55 仍如此） |
| 应用行为 | 22:36 两次启动均被判 `enhanced-mode-utun` 模式（说明应用的路由检测看到 TUN） |

**自验方法**：关闭 Surge 主界面的增强模式开关后，执行 `/sbin/route -n get 31.94.76.9`——若显示 `en0` 才是真的关掉了；显示 `utun8` 就是还开着。可能的情况：开关在之前就处于开启状态、或界面开关与实际状态不同步。

### 事实二：纯配置模式下 VoWiFi 无法工作（实验证明，非配置问题）

两个相互独立的死因：

**死因 1：Surge 配置模式不能转发 Proletarius 的 IKEv2/UDP**
- 实测 Surge SOCKS5（6582 端口，8.31 配置）UDP ASSOCIATE 返回 **rep=0x07（不支持）**——这是 Surge 产品行为（UDP 一律由增强模式 TUN 接管，SOCKS5 入口不提供 UDP 转发）
- Proletarius 的 IKEv2 是裸 UDP socket：系统代理只对"主动走代理的 TCP 应用"生效，裸 UDP 根本不经过 Surge
- → **8.31 里精心配置的 VoWiFi 分流规则，在配置模式下对 Proletarius 完全不生效**（规则只对进入 Surge 的流量有意义，即增强模式）

**死因 2：你的网络直连英国 ePDG 完全不可达**

我构造了结构合法的 IKE_SA_INIT 报文（SA + KE(DH-14, 256B) + Ni，素数取自项目 swu-go 库）做可达性实验：

| 实验 | 结果 |
|---|---|
| 对照组：NTP (UDP 123) 绑定 en0 | ✓ 48 字节响应——UDP 机制正常 |
| 实验组：IKE 直连 31.94.76.5/6/9/10 × 端口 500/4500，绑定 en0 | **8 次探测全部无响应** |

（第一版探测曾因手工素数错误导致报文畸形、结论无效；本版已用 swu-go 内嵌的 RFC 3526 group-14 标准素数修正，结论可信。）

配置模式下 Proletarius 的传输选择链：SOCKS5 UDP 探测失败（死因 1）→ 路由查不到 utun → 只剩 `direct` 模式 → 直连被阻断（死因 2）→ **必然失败**。

### 事实三：当前增强模式路径的断点已精确定位——节点「🇬🇧 英国 03」不支持 UDP

- 通过 Surge HTTP API 查询：`🇬🇧 VoWiFi-UK` 组（smart 类型）当前自动选中 **「🇬🇧 英国 03」**
- Surge 的 UDP 中继基础设施正常：请求日志中 NetBIOS UDP 会话经香港节点以 "Port Map mode" 正常转发
- 但我的 IKE 探测走同一条链路（TUN → 规则 `IP-CIDR,31.94.0.0/16 → 🇬🇧 VoWiFi-UK` → 英国 03）→ **无任何响应**
- 结合 8.31 配置的 `udp-policy-not-supported-behaviour = REJECT`：**该节点不支持 UDP 转发 → Surge 静默丢弃 IKEv2 包 → 隧道无法建立**
- 这精确复现了应用 22:30 / 22:36×2 的三次失败（12s/7s/5s 后 Session stopped，期间无任何 ePDG 协商日志）

8.31 的规则本身写得完全正确（`epdg.epc.mnc033.mcc234.pub.3gppnetwork.org → 🇬🇧 VoWiFi-UK` + `IP-CIDR,31.94.0.0/16` + `always-real-ip` 含 3gppnetwork.org）——**规则没问题，是 smart 组按 HTTP 延迟选中的节点没有 UDP 能力**。

---

## 1. 回答你的核心问题

> "我不想开 surge 增强模式只想使用配置模式，如果一定要开启增强模式请告知我"

**必须如实告知：如果只使用 Surge（配置模式），VoWiFi 无法工作**——这不是配置能解决的，是路径不存在（事实二的两个死因）。你有两个真正可行的选择：

- **方案 A**：接受增强模式 + 修复节点 UDP 问题（改一行配置）
- **方案 B**：保持 Surge 配置模式 + 并行运行一个支持 SOCKS5 UDP 的内核（mihomo），在 Proletarius 应用内显式指定代理地址——**这条路径不需要增强模式**，而且就是你今天上午三次成功所走的路径

---

## 2. 修复方案

### 方案 A：增强模式 + 钉住支持 UDP 的英国节点（推荐，改动最小）

1. 在 Surge 面板核对各英国节点的 UDP 支持标记，选出一个支持 UDP 的节点
2. 修改 8.31 配置 `[Proxy Group]` 段——把 smart 改为 select：

```ini
# 修改前
🇬🇧 VoWiFi-UK = smart, include-other-group=📦 Nodes, policy-regex-filter=(?i)(🇬🇧|英国|United Kingdom|\bUK\b|London), no-alert=1, hidden=1, include-all-proxies=0

# 修改后
🇬🇧 VoWiFi-UK = select, include-other-group=📦 Nodes, policy-regex-filter=(?i)(🇬🇧|英国|United Kingdom|\bUK\b|London), no-alert=1, hidden=1, include-all-proxies=0
```

3. 在 Surge 面板的 🇬🇧 VoWiFi-UK 组手动选中该 UDP 节点

**为什么必须改 select**：smart 组按 HTTP 延迟自动选节点、不测试 UDP 能力——即使今天手动换好，明天延迟波动后可能又自动切回无 UDP 节点，问题随机复发。

**若所有英国节点都不支持 UDP**：把 ePDG 规则指向任一支持 UDP 的其他地区节点（如香港组中已证实转发 UDP 的节点）——ePDG 不挑来源 IP 的国家，香港节点连英国 ePDG 完全可行。

**验证**：改完后重新启动 Wi-Fi Calling，日志应在 1~2 秒内出现 `swu/state_init.go ePDG 支持 IKE Fragmentation`，随后 `Tunnel ready` + `IMS registered`。也可先让我重跑 IKE 探测脚本确认链路通。

### 方案 B：Surge 配置模式 + 并行 mihomo + 应用内指定代理（满足"不开增强模式"）

前提发现：Proletarius 支持在 VoWiFi 请求里显式指定代理（`reader_vowifi_api.go:154`：`Proxy` 字段"可选：填写时优先使用，留空时尝试网络直连"；实际回落顺序是：显式设置 → 系统 SOCKS 代理）。

1. 本地运行 mihomo / Clash 系内核，监听 `127.0.0.1:7897`（沿用你今早的节点配置——今早 09:43/12:56/22:09 三次成功正是走这条路）
2. **真正关闭 Surge 增强模式**（用上面的自验方法确认 utun 路由消失）
3. 在 Proletarius 的读卡器 VoWiFi 设置中把代理地址显式填为 `127.0.0.1:7897`（而不是留空让它去探测系统代理——系统代理指向 Surge 6582，而 Surge 不支持 UDP ASSOCIATE）
4. 系统代理继续指向 Surge（6528/6582），Surge 以配置模式服务其他所有流量

结果：VoWiFi 经 mihomo 的 SOCKS5 UDP relay（XUDP）到节点 → ePDG；其余流量走 Surge 配置模式。两不干扰。

**注意**：mihomo 所用节点必须支持 UDP（今早的节点满足）。

### 方案 C：不用任何代理直连 ePDG —— 已被实验排除，不可行

---

## 3. 实验记录附录

| # | 实验 | 方法 | 结果 |
|---|---|---|---|
| E1 | Surge SOCKS5 UDP ASSOCIATE（6153，旧配置） | SOCKS5 握手 + CMD=0x03 | rep=0x07 拒绝 |
| E2 | Surge SOCKS5 UDP ASSOCIATE（6582，8.31） | 同上 | rep=0x07 拒绝 |
| E3 | ePDG DNS 解析（系统/8.8.8.8 对照） | nslookup | 一致且为真实 IP（31.94.76.5/6/9/10），无污染 |
| E4 | NTP over en0（对照组） | 绑定 en0 发 UDP 123 | ✓ 响应正常 |
| E5 | IKE 直连 ePDG over en0 | 合法 IKE_SA_INIT ×4 IP ×2 端口 | 全部无响应（直连不可达） |
| E6 | IKE 经 Surge TUN → 规则 → 英国 03 | 默认路由发包 | 全部无响应（节点 UDP 缺失） |
| E7 | Surge 近期请求 UDP 记录 | HTTP API /v1/requests/recent | UDP 会话可被记录（NetBIOS 例证）；应用失败时无 ePDG 响应与之吻合 |
| E8 | VoWiFi-UK 当前选中 | HTTP API /v1/policy_groups/select | 「🇬🇧 英国 03」 |

IKE 探测报文构成：28B 头 + SA(AES_CBC-128/PRF-SHA2-256/INTEG-SHA2-256-128/DH-14) + KE(256B, RFC3526 group-14 素数取自 `backend/third_party/swu-go/pkg/crypto/diffiehellman.go`) + Ni(32B)，共 355 字节——与 ePDG 此前协商成功的算法组合一致。

---

## 4. 日志时间线（完整复盘）

| 时间 | 代理环境 | 传输模式 | 结果 |
|---|---|---|---|
| 09:43 / 12:56 / 22:09 | 系统代理=7897（mihomo，支持 SOCKS5 UDP） | socks5-udp-domain | ✅ 隧道+IMS 注册+通话 |
| 22:30 | 换 Surge（默认配置 6152/6153） | enhanced-mode-utun | ❌ 12s 无响应 |
| 22:36 ×2 | 换 Surge 8.31（6528/6582） | enhanced-mode-utun | ❌ 7s / 5s 无响应 |

传输模式变化的机制：Rust 端 `transport_selection` 先探测系统 SOCKS 的 UDP ASSOCIATE（7897 成功→socks5-udp-domain；Surge 失败）→ 失败后本地解析 ePDG → 检查路由 → 增强模式下路由到 utun8 → 选 enhanced-mode-utun（绑定 utun8 发包，ePDG 流量交给 Surge 规则处置）。

---

*本报告为只读排查 + 独立网络实验，未修改任何应用代码与用户配置。Surge API 仅做只读查询。*