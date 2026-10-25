---
AIGC:
  ContentProducer: '001191110102MAD55U9H0F10002'
  ContentPropagator: '001191110102MAD55U9H0F10002'
  Label: '1'
  ProduceID: '869da96e-7fb8-40a5-9f4d-6206c4648bfe'
  PropagateID: '869da96e-7fb8-40a5-9f4d-6206c4648bfe'
  ReservedCode1: '10e3ab87-a02c-487f-95d7-c272534e74cb'
  ReservedCode2: '10e3ab87-a02c-487f-95d7-c272534e74cb'
---

# VoWiFi 启动失败排查报告（Surge 增强模式场景）

- **日期**：2026-09-19
- **现象**：开启 Surge 代理后，项目启动 Wi-Fi Calling 失败，IKEv2 隧道无法建立
- **排查方式**：日志时间线分析 + 传输选择代码审读 + 系统网络状态实测（路由/代理/DNS/SOCKS5 探测）

---

## 1. 结论（一句话）

**Surge 的本地 SOCKS5 服务器不支持 UDP ASSOCIATE（实测返回 rep=0x07），项目因此从原来可用的 `socks5-udp-domain` 模式回退到 `enhanced-mode-utun` 模式——把 IKEv2 UDP socket 绑定到 Surge 的 utun8、将 ePDG 流量全权交给 Surge 的分流规则；而 Surge 规则中没有给 ePDG 的 UDP 500/4500 安排可用出口（命中 FINAL 走了不支持 UDP 的策略或被拒绝），IKE INIT 无响应，隧道建不起来。**

---

## 2. 证据链

### 2.1 日志时间线

| 时间 | 传输模式 | 代理 | 结果 |
|---|---|---|---|
| 09:43 / 12:56 / 22:09 | `socks5-udp-domain` | 127.0.0.1:**7897**（Clash 系） | ✅ IKEv2 建立 → IMS 注册 → 通话成功 |
| 22:30:06（本次） | `enhanced-mode-utun` | 127.0.0.1:**6153**（Surge） | ❌ 12 秒无 IKE 响应 → Session stopped |

- 成功案例：启动 1 秒内即出现 `swu/state_init.go:191 ePDG 支持 IKE Fragmentation` 协商日志
- 失败案例：`Progress 2/Access` 之后 **12 秒内无任何 swu 日志**（IKE INIT 发出后石沉大海），直接停止
- 22:09~22:30 之间系统代理从 7897（Clash）换成了 6152/6153（Surge）——失败随之出现

### 2.2 实测数据

| 检测项 | 结果 |
|---|---|
| `scutil --proxy` | HTTP 127.0.0.1:6152 / SOCKS 127.0.0.1:6153（Surge 已接管系统代理） |
| 路由表 | `1/8、2/7、4/6、8/5 → 10.254.1.1 → utun8`——Surge 增强模式（TUN）拆分路由，劫持全网流量 |
| `route -n get 31.94.76.9`（ePDG IP） | **interface: utun8**（ePDG 流量也被 Surge TUN 接管） |
| ePDG DNS 解析 | 系统 resolver 与 8.8.8.8 直查结果一致（31.94.76.5/6/9/10，Orange UK 真实 ePDG）——**无 DNS 污染、非 fake-ip** |
| **Surge SOCKS5 UDP ASSOCIATE 探测** | **rep=0x07（不支持）**——Surge 产品特性：UDP 由增强模式 TUN 接管，本地 SOCKS5 不暴露 UDP 转发 |
| 旧代理 7897 UDP ASSOCIATE | 支持（现已停用）——这是之前三次成功的基础 |

### 2.3 传输选择逻辑（代码路径）

Rust 端 `transport_selection.rs` + `proletarius-vowifi/lib.rs:70` 的选择顺序：

```
1. probe_proxy(6153) UDP ASSOCIATE → 失败（rep=0x07）→ 无法提前锁定 socks5-udp-domain
2. resolve_epdg → 真实 IP 31.94.76.x（非 fake-ip）
3. route -n get → utun8 → select_proxy_mode 第 2 分支命中：
   EnhancedUtun { address: 31.94.76.x, interface: "utun8" }
4. Go 端（reader_vowifi_transport.go:43-48）：
   UDP socket 经 IP_BOUND_IF 绑定 utun8 → IKEv2 包注入 Surge TUN
```

之后 ePDG 的 UDP 500/4500 由 **Surge 的分流规则**决定命运：
- 命中 FINAL → 代理策略：节点需支持 UDP 转发，多数节点禁 UDP → 包被丢弃
- 命中 REJECT：直接丢弃
- 即使 DIRECT，也依赖 Surge TUN 的 UDP 会话回程质量（实测本环境不通）

而成功的旧路径（Clash 7897）：`socks5-udp-domain` → mihomo UDP ASSOCIATE + XUDP 封装 → 节点 UDP 出口 → ePDG 回包经代理送回应用层 relay——完全绕开内核路由，闭环可靠。

---

## 3. 修复方案

### A. 立即可用（改 Surge 配置，不动代码）

**A1（推荐）：给 ePDG 流量加分流规则，指向支持 UDP 的策略**

```ini
[Rule]
DOMAIN-SUFFIX,3gppnetwork.org, <支持UDP的策略名>
IP-CIDR,31.94.76.0/24, <支持UDP的策略名>, no-resolve
```

- enhanced-mode-utun 模式下 Surge 收到 IKEv2 后会按规则经该策略转发 UDP——等效替代原 SOCKS5 UDP relay 路径
- 前提：所选策略的节点支持 UDP（沿用之前 Clash 那类节点即可）
- **验证**：保存规则后重启 Wi-Fi Calling，日志应在 1-2 秒内出现 `ePDG 支持 IKE Fragmentation`，随后 `Tunnel ready` + `IMS registered`

**A2：若想直连 ePDG，规则改 DIRECT**

```ini
DOMAIN-SUFFIX,3gppnetwork.org, DIRECT
```

- 风险：国内直连英国 ePDG 的 UDP 500 可达性未知（此前所有成功均经代理）
- 同样建议同时加 IP-CIDR 规则（Surge 对纯 IP 的 UDP 流量按 IP 规则匹配）

**A3：不动 Surge——本地并行跑支持 SOCKS5 UDP 的前端**

- 起一个 mihomo/Clash 系监听 7897（节点配置沿用），系统代理指回它，或让 Proletarius 使用该地址
- 项目探测 UDP ASSOCIATE 成功后自动恢复 `socks5-udp-domain` 模式，行为与之前完全一致

### B. 代码改进（项目层面，建议排期）

| 编号 | 改进 | 说明 |
|---|---|---|
| B1 | enhanced-mode-utun 失败自动降级 | IKE INIT 超时（当前 12s 硬停）后自动尝试 direct（绑定物理接口 en0），避免单一路径不通即失败 |
| B2 | 传输模式手动覆盖 | UI 提供 direct / socks5 / auto 三态开关——"探测最优"实际不通时用户可自救 |
| B3 | 诊断信息增强 | 选中 enhanced-mode-utun 时日志输出 address+interface+"ePDG 流量经 Surge 增强模式转发，请确保规则允许 UDP 500/4500"；把 probe 的 rep=0x07 透传 UI（"Surge 不支持 SOCKS5 UDP 转发"） |
| B4 | 文档化三种场景 | Surge（TUN 接管）/ Clash（SOCKS5 UDP）/ 无代理（direct）的传输行为与配置要求 |

### 建议操作顺序

1. 先试 **A1**（Surge 加规则指向支持 UDP 的策略）——最贴近原来的成功路径
2. 不通再试 **A2**（DIRECT）
3. 都不行用 **A3**（mihomo 并行）
4. 代码层把 **B1 + B3** 排入下一批修复（B3 改动最小，B1 涉及 session 降级逻辑）

---

## 4. 附：当前环境快照

- Surge 增强模式 TUN：utun8（10.254.1.1，MTU 1500），拆分路由 `1/8、2/7、4/6、8/5、16/4...`
- ePDG：`epdg.epc.mnc033.mcc234.pub.3gppnetwork.org`（Orange UK，234-33）→ 31.94.76.5/6/9/10
- IMS 隧道内：IPv6-only（2a00:23ee:...），P-CSCF 2a01:4c8:f000:faa/f52/f69::1，MTU 1280
- 项目传输模式决策全部在 Rust 端（`v0.16-proxy-preferred-direct` 契约），Go 端只执行

*本报告为只读排查，未修改任何代码与配置。*