# Proletarius VoWiFi 认证修复方案

> **历史资料，已失效。** 本文针对已删除的 Go 后端与早期 CTExcel 实现，文中的 `go build`、直连重试及文件行号不适用于当前 Switch。现行 Rust 构建和代理约束见 [Rust 后端说明](rust/README.md)，SIM World 实卡注册证据见 [全 Rust 验收记录](docs/optimization/all-rust/iteration-557-simworld-nat-notify-registration.md)。

> 基于 iOS 27.0 IPSW (iPhone18,1_27.0_24A5418b) 全球运营商 Carrier Bundle 提取的真实参数
> 对象：CTExcel UK 卡（MCC=234, MNC=033, IMSI prefix 23433, Orange_uk bundle）
> 目标：高效稳定地完成 IKEv2 → EAP-AKA → ePDG 隧道 → IMS SIP REGISTER 全链路

---

## 一、当前故障根因（代码级定位）

### 1.1 digest.go — Digest 认证硬伤（P0，直接导致 401 死循环）

| 行号 | 缺陷 | 影响 |
|---|---|---|
| L56-61 | `algorithm` 为空时直接报错 | P-CSCF 省略 algorithm（RFC 2617 默认 MD5）时直接失败 |
| **L62-64** | **只接受 `AKAv1-MD5`，拒绝普通 `MD5`** | **EE P-CSCF 返回 `algorithm=MD5` → `unsupported digest algorithm`** |
| **L226-228** | **nonce 解码后必须 ≥32 字节** | **EE P-CSCF 返回 16 字节 hex nonce → `invalid AKA nonce`** |
| L282 | `buildDigestAuthorization` 硬编码 `algorithm=AKAv1-MD5` | 回应头 algorithm 不匹配 |
| L311-315 | `digestResponse` 把 RES 当密码写入 HA1 | MD5 场景无 RES |

### 1.2 security.go — IPsec 激活链路断裂（P1）

| 行号 | 缺陷 | 影响 |
|---|---|---|
| **L441-443** | **`expandIPSecKeys` 硬性要求 CK/IK 各 16 字节** | MD5 场景无 CK/IK → IPsec 永不激活 |
| L912-922 | `emptyDigestAuthorization` 硬编码 `AKAv1-MD5` | 首次 REGISTER 空摘要头不匹配 |

### 1.3 provider.go — 认证循环与安全配置（P1）

| 行号 | 缺陷 | 影响 |
|---|---|---|
| **L25** | **`maxAuthenticationChallenges = 3`** | 仅 4 次尝试，stale 重试不够 |
| L906-910 | `len(material.auts)==0 && useSecurity` 才激活 IPsec | MD5 场景不触发 |
| L899-901 | 只有 AT&T 310280 才用 `isim_strict` | 其他运营商无法走 ISIM |
| **L414** | **`deriveIdentities` 从 IMSI 构造 IMS 域** | **MNC=033 → `ims.mnc033.mcc234` 但 P-CSCF realm 是 `mnc030`** |

### 1.4 reader_vowifi_api.go — 部署配置（P2）

| 行号 | 缺陷 | 影响 |
|---|---|---|
| L399 | `SecurityMode: SecurityDisabled` | IPSW 显示 `UseIPSec: true` |
| L282 | IKE Config 无 `LocalIdentifier` | Orange_uk 要求 `0$imsi@nai.epc.mnc$mnc.mcc$mcc.3gppnetwork.org` |

### 1.5 reader_vowifi_aka_adapter.go — AKA 适配器（P1）

| 缺陷 | 影响 |
|---|---|
| 未实现 `PreferredAKAProvider` 接口 | digest.go L183 检查此接口，无法走 ISIM |
| 无读取 ISIM 静态密码能力 | MD5 场景无密码注入 |

---

## 二、IPSW 参数对照（Orange_uk.bundle 真实配置）

### 2.1 IKE/ePDG 参数

| 参数 | IPSW 值 | 代码现状 |
|---|---|---|
| ePDG 地址 | `epdg.epc.mnc033.mcc234.pub.3gppnetwork.org` | 动态构造 ✅ |
| IKE 加密 | **AES-128** | 默认 AES-256 ❌ |
| IKE 完整性 | SHA2-256 | 需核对 |
| DH 组 | 14 | ✅ |
| PRF | SHA2-256 | 需核对 |
| IKE 生命周期 | 80000s | 需核对 |
| EAP 方法 | EAP-AKA | ✅ |
| 认证方法 | None (纯 EAP) | ✅ |
| **LocalIdentifier** | `0$imsi@nai.epc.mnc$mnc.mcc$mcc.3gppnetwork.org` | **未设置 ❌** |
| 验证远端证书 | false | 需核对 |
| NATT KeepAlive | enabled, 20s | ✅ |
| DPD | disabled | 需核对 |

### 2.2 Child SA

| 参数 | IPSW 值 |
|---|---|
| 加密 | AES-128 |
| 完整性 | SHA2-256 |
| DH 组 | 14 |
| 生命周期 | 86400s |
| ReplayWindowSize | 12 |

### 2.3 IMS/SIP 参数

| 参数 | IPSW 值 | 代码现状 |
|---|---|---|
| **UseIPSec** | **true** | **`SecurityDisabled` ❌** |
| **认证算法** | **未指定（=MD5）** | **只支持 AKAv1-MD5 ❌** |
| **IMS 域** | **`ims.mnc030.mcc234.3gppnetwork.org`** | **构造 `mnc033` ❌** |
| EnableWiFiCallingWithoutEntitlement | true | 不检查 entitlement ✅ |

### 2.4 关键发现：MNC 路由差异

SIM 卡 IMSI = 23433... (MCC=234, MNC=033, Orange UK)

但 Orange_uk.bundle 的 `supported_plmns` = **["23430"]**

- ePDG 地址使用 MNC=**033**（SIM 卡原始 MNC）
- IMS 域/P-CSCF realm 使用 MNC=**030**（EE 核心网统一 MNC）
- 代码当前 `deriveIdentities()` 从 IMSI 构造域 → `ims.mnc033.mcc234` ← **错误**

### 2.5 加密身份证书（EE 共享）

| 参数 | 值 |
|---|---|
| 证书 CN | `live_wifioneaa.ee.co.uk` |
| 序列号 | `e1bde644abadf381` |
| `ValidateRemoteCertificate` | false |
| 用途 | IKE encrypted_identity（EAP-AKA 不需要） |

---

## 三、修复方案（按优先级排序）

### 修复 1: digest.go — 支持普通 MD5 Digest（P0）

#### 1a. parseDigestChallenge — 接受 MD5

```
L56-64 改为:
  algorithm 为空 → 默认 "MD5"
  接受: MD5, MD5-SESS, AKAv1-MD5, AKAv2-MD5
  拒绝: 其他
```

#### 1b. decodeAKANonce → decodeNonce — 短 nonce 不报错

```
解码函数只负责 base64/hex 解码，不做长度检查
长度检查移到 AKA 路径内部
增加 hex 解码尝试（EE P-CSCF 用 hex nonce）
```

#### 1c. authenticateAKA → authenticateDigest — MD5 跳过 AKA

```
algorithm=MD5/MD5-SESS/"" → 走密码路径
  尝试读 ISIM 静态密码
  无密码 → 空密码（隧道内安全）
algorithm=AKAv1-MD5/AKAv2-MD5 → 走现有 AKA 路径
  nonce < 32 → 报错
  nonce ≥ 32 → RAND‖AUTN → USIM/ISIM → RES/CK/IK
```

#### 1d. akaMaterial 增加 password + isMD5 字段

#### 1e. digestResponse — 支持 MD5 密码

#### 1f. buildDigestAuthorization — 动态 algorithm

### 修复 2: provider.go — 认证循环与身份域（P0）

#### 2a. maxAuthenticationChallenges = 5

#### 2b. carrier_profiles.json 添加 Orange UK（route MNC=030）

#### 2c. register() — MD5 场景不调 AKA 不激活 IPsec

```
if !material.isMD5 && len(material.auts)==0 && useSecurity:
    activateIPSec(ck, ik)
```

### 修复 3: security.go — IPsec 兼容 MD5（P1）

#### 3a. expandIPSecKeys — MD5 场景优雅失败

#### 3b. emptyDigestAuthorization — 动态 algorithm

### 修复 4: reader_vowifi_api.go — 部署配置（P1）

#### 4a. IKE LocalIdentifier = `0$imsi@nai.epc.mnc{mnc}.mcc{mcc}.3gppnetwork.org`

#### 4b. SecurityMode 改为 SecurityOptional

#### 4c. 注入 ISIM 密码（如可用）

### 修复 5: aka_adapter.go — 实现 PreferredAKAProvider（P2）

### 修复 6: carrier_profiles.json — Orange UK 条目（P1）

---

## 四、执行顺序

| 阶段 | 修复项 | 文件 | 工作量 | 优先级 |
|---|---|---|---|---|
| Phase 1 | Orange UK carrier profile | carrier_profiles.json | 10min | P0 |
| Phase 2 | digest.go: MD5 + 短 nonce | digest.go | 45min | P0 |
| Phase 3 | provider.go: 认证循环 + 域 | provider.go | 30min | P0 |
| Phase 4 | security.go: IPsec 兼容 | security.go | 20min | P1 |
| Phase 5 | reader_vowifi_api.go: 配置 | reader_vowifi_api.go | 15min | P1 |
| Phase 6 | aka_adapter.go: ISIM | aka_adapter.go | 30min | P2 |
| Phase 7 | 编译 + 部署 + 测试 | — | 20min | — |
| **合计** | | | **~2.5h** | |

---

## 五、修复后认证流程

```
1. SIM 就绪 (IMSI=23433..., MCC=234, MNC=033)
2. Carrier Profile 匹配 → orange-uk-23433
   route: MCC=234, MNC=030 (IMS 域用 mnc030)
   ePDG: epdg.epc.mnc033.mcc234.pub.3gppnetwork.org
3. IKEv2 SA_INIT → ePDG
   LocalID: 0$imsi@nai.epc.mnc033.mcc234.3gppnetwork.org
   Proposal: AES-128/SHA2-256/DH-14/PRF-SHA2-256
4. IKE_AUTH → EAP-AKA (USIM → RES/CK/IK → ePDG)
5. 隧道建立 → inner IP + P-CSCF
6. SIP REGISTER → P-CSCF
   From: sip:23433xxx@ims.mnc030.mcc234.3gppnetwork.org (MNC=030)
   Authorization: Digest algorithm=MD5, nonce="", response=""
7. 401 Challenge: algorithm=MD5, nonce=<16B hex>, realm=ims.mnc030.mcc234.3gppnetwork.org
8. MD5 Digest: MD5(username:realm:password:nonce:nc:cnonce:qop:uri)
   password = ISIM 静态密码 or 空密码
9. 200 OK → IMS 注册成功
10. SMS/Call 就绪
```

---

## 六、风险与缓解

| 风险 | 可能性 | 缓解 |
|---|---|---|
| ISIM 无静态密码 → 空密码被拒 | 中 | 尝试读 ISIM EF 密码；空密码先试；被拒再尝试 AKA-derived |
| P-CSCF 要求 IPsec 但 macOS 无 XFRM | 高 | SecurityOptional 降级；隧道内 ESP 保障安全 |
| MNC 路由不匹配 | 已确认 | carrier_profiles route override (mnc=030) |
| IKE Proposal 不匹配 (AES-128 vs 256) | 中 | AutoProposalFallback 自动降级 |
| Shadowrocket TUN 不转发 IKE UDP | 已知 | 现有重试机制 + 直连 UDP |

---

## 七、验证清单

- [ ] `go build ./...` 编译通过
- [ ] 插入 ESTKme + CTExcel UK 卡
- [ ] 日志: IKE 隧道已建立
- [ ] 日志: P-CSCF 返回 401, algorithm=MD5
- [ ] 日志: MD5 Digest 认证成功
- [ ] 日志: IMS 注册成功: registered=true
- [ ] 测试 SMS 收发
- [ ] 测试通话
- [ ] /tmp/proletariusd-vowifi.log 不再出现 401 stale 循环
