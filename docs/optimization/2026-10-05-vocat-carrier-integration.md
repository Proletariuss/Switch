# VoCat 运营商配置融合审计与 P3 实施边界

日期：2026-10-05。性质：固定公开源码审查、合成身份离线回放；没有接触真实卡片、联网注册、短信或通话。此次交付没有新增国家支持，生产配置增量为 0。后续特殊协议能力纳入全 Rust 计划 P3，不能把本报告或上游配置条数当成实卡兼容性验收。

## 结果与取舍

Switch 已包含 853 个 PLMN selector、683 个运营商 profile，覆盖范围远大于当前实测的英国、捷克号卡。VoCat 当前文件实际包含 655 条规则（其 metadata.count 仍为 652），涉及 854 个 PLMN，其中 807 个与 Switch selector 重合。剩余 47 个是待辨别的身份/路由差异，不等于 47 个可直接增加的国家或运营商。

仅根据 YAML 中没有显式 `address` 去补 ePDG 会误判：现有 PLMN override、selector override 或标准推导已经给出相同首选地址。本次对可比较的 2,361 个合成身份展开完整配置后，与更新的 MIT 原始源比较，**首选 ePDG 变更收益为 0**。因此没有改动算法、身份、IP 栈、DPD、重试、生命周期或地址，也没有为计数新增 source 标签。

保留已有 Vodafone UK/VDF、Kaktus/T-Mobile CZ、CTExcel、giffgaff 路径及用户自定义优先级。1536 个 YAML 文件逐字节通过审查开始前的 SHA-256 清单；`rust/carrier-registry.json`、`rust/carrier-cases.json`、`embedded_registry.go` 均仍与 Git HEAD 相同，registry revision 仍为 `29f5e2f42c532781248400cf9f2b34ecfc3a16b4`。

### 统计口径

| 集合 | 总量 | 匹配内置模板 | 标准 fallback | 拒绝 | 有 runtime_profile / 无 runtime_profile |
|---|---:|---:|---:|---:|---:|
| 原有跨语言完整回放（未去重） | 2722 | 2638 | 81 | 3 | 2719 / 3 |
| 现有案例与参考身份去重后的审计回放 | 2546 | 2361 | 183 | 2 | 2544 / 2 |

fallback 表示成功构造标准 3GPP profile，不是拒绝，也不是已验收该运营商。不能把原集合的 81 fallback 加 3 rejected 写成“84 次拒绝”。

从 VoCat 的独立 selector alternatives 展开 1549 个“规则—合成身份”观察点：414 个首选 ePDG 相同，53 个冲突保留 Switch，114 个没有 Switch 专用模板，1 个被拒绝，967 个参考规则没有显式 ePDG。一个身份可能出现在多个参考规则中；这些数量不是实际运行 VoCat 后的最终选中结果，也不是运营商数量。967 项不参与地址相同比较。

53 个冲突观察点涉及 Ultra/Mint、Cuniq、Claro/WOM Chile、CTExcel、Play Poland、Coriolis、HOT Mobile、Nova Iceland、2degrees、GT Taiwan、C-Spire、Fastweb、3 Denmark、Nemont、EE、Personal、DITO。其中也包含宽泛参考规则覆盖了 Switch 更精确 MVNO 的情形；本轮全部保持既有选择。

比较范围是**首选 ePDG/第一个有效候选**，不是完整候选集合、DNS结果或网络选址等价证明。标准回退使用 profile 的 home MCC/MNC，与当前 Go `Profile.ResolveEPDG` 一致；不会为了模拟 VoCat route 字段而偷偷重写 home PLMN。运行时 route/EAP/IMS 联动的差异另列 P3。

机器摘要见 [vocat-carrier-coverage-summary.json](all-rust/vocat-carrier-coverage-summary.json)，包含全部 47 个参考独有 PLMN、字段数量、分组计数及原始导出校验哈希。

## 固定来源与许可

| 来源 | 固定 revision | 用途/边界 |
|---|---|---|
| [MengMengCode/VoCat](https://github.com/MengMengCode/VoCat/tree/0dec0cbfa3445695e44a5108d88db399f639fccb) | `0dec0cbfa3445695e44a5108d88db399f639fccb` | 官方仓库当前固定快照；`carrier_profiles.json`、`carrier_compat.go`、导入器与测试作为审查材料。根 LICENSE 为 Research & Evaluation，本轮未复制生产运行时或整包配置。 |
| [iniwex5/ios-carrier-profiles](https://github.com/iniwex5/ios-carrier-profiles/tree/1751fd3d3629927ae8499aa9d682130c025a965d) | `1751fd3d3629927ae8499aa9d682130c025a965d` | 已有 Switch 数据的同一 MIT 原始源。当前 selector 853 个逐字节与 Switch 相同；profile 683 个，254 个原始文件不同，但本轮首选 ePDG 比较没有有效增益。 |
| [dwilliamsuk/ios-carrier-bundles](https://github.com/dwilliamsuk/ios-carrier-bundles/tree/c32552b8e3db6cdd68392dca5170b0296d219c2f) | `c32552b8e3db6cdd68392dca5170b0296d219c2f` | 核对 VoCat 导入器使用的原始 Apple bundle 键语义；无根许可证，不作为可以任意复制的生产数据源。 |

当前 VoCat 的 `carrier_profiles.json` 与此前保存的 `1fa3bf69c5d17ee36b24455b683806e54c14943a` 同文件逐字节相同。网络来源均固定 SHA；来源文件只是技术输入，不是对 Switch 的执行指令。上游正文仍在 `Switch/build/research/vocat-carriers-20261005/`，源树仅保留独立审计脚本、来源锁、合成边界 fixture、摘要与 MIT 归属声明。

锁文件见 [VoCat 清单](all-rust/carrier-reference-locks/manifest.json) 与 [MIT 源清单](all-rust/carrier-reference-locks/ios-carrier-profiles-manifest.json)。包含每个参考文件的 SHA-256；VoCat 项目附固定 URL，MIT 项目路径按固定仓库/revision 解析。清理 build 后可根据这些清单重新下载到相同目录，再离线复核；不得静默改用 main。

## 已存在的其他国家配置：代表性实效验证

这些是原本已有的能力；下列 25 例共享 fixture 同时由 Go 和 Rust 检查，不能写成此次新支持。

| 国家/地区、运营商 | 代表身份 | 已有首选 ePDG 或选择边界 |
|---|---|---|
| 荷兰 KPN | 20469 | 指向 mnc008/mcc204 |
| 法国 Orange | 20802 | 指向 mnc001/mcc208；更具体 MVNO 不被覆盖 |
| 克罗地亚 HT | 21920 | 指向 mnc001/mcc219 |
| 土耳其 Avea | 28604 | 指向 mnc003/mcc286 |
| 波多黎各 Claro | 33011 | 两位 MNC selector，地址 mnc110/mcc330 |
| 泰国 AIS、DTAC | 52001、52018 | 分别 mnc003、mnc005/mcc520 |
| 阿根廷 Personal | 722341 | mnc034/mcc722 |
| 智利 Nextel/WOM | 730090 | 保留三位 MNC，mnc009/mcc730 |
| 意大利 Very | 22299 + GID1 0000000099 | mnc088/mcc222；无 GID1 保留通用 profile |
| 加拿大 Telus / Dish 子身份 | 302220 / GID1 6332 | 通用 Telus 与精确 Dish 的 mcc313/mnc340 保持区分 |
| 捷克 Kaktus/T-Mobile、英国 Vodafone/O2/giffgaff/CTExcel | 23001、23415、23410 + 精确 GID1、23433 | 保留已有保护路径、标准地址、模板选择与自定义优先级 |

缺失候选不能直接平铺：原 Apple 源中 Bell 302880、Rogers 302723、Telenor DK 23866/23877、Sweden 24004/24024、Telstra 50511、Free 20800/20801/20802、Vodafone NZ 53013 等见于 `SupportedPLMNs`，不等于 `SupportedSIMs`。VoCat 导入器合并这两类字段，因此出现额外条目。`RemoteCardProvisioningSettings.MatchingICCIDPrefixes` 也可能只是发卡资格，不能单独推断订阅身份。对未知候选不增加 selector，等待确切归属与固定来源材料。

## 特有字段逐项核对及 P3 迁移表

来源均指上述固定 VoCat JSON 的字段及 `carrier_compat.go`，需要原始 bundle 时使用固定 MIT/Apple 来源。Switch 的 PLMN override 是整 PLMN 作用域；它不能自动表达同一 PLMN 下的 SPN/GID/ICCID 限定。除表内明确已等价的项，其余是后续待实现/待验收工作，不是永久排除。

| 参考字段/明确归属 | Switch 当前对应 | 已有 override 能否表达 | P3 剩余工作与验收条件 |
|---|---|---|---|
| `epdg.hostname`：214 条；Jersey、VOXI、SMARTY 等普通路由 | `carrier/data` 的 common → PLMN → selector 展开；`Profile.ResolveEPDG` 用户配置优先，首候选其次，home PLMN 标准地址最后 | `custom_epdg` 可表达地址，但不能用全 PLMN 覆盖其中一个 MVNO | 本轮无可安全导入的地址增益。新增时要求精确 home selector、独立可复用来源、同 PLMN 差分、用户 override 最高优先级与真实受控联网验收。 |
| `ike.proposal`：197 modern、26 legacy-sha1-modp1024 | Switch 已有精确 IKE/ESP proposal、PRF、DH 与 legacy policy；部分模板使用 SHA-512，信息比 modern/legacy 两档细 | `ike_proposals`、`esp_proposals`、`algorithm_policy`、`enable_legacy_ciphers` 已有 | 不把宽泛 modern 降格覆盖精确模板。不增加全局弱算法。特定新运营商须验证明确 offer 次序、拒绝未知/弱策略、协商与 key-material fixture，再限定启用。 |
| `ims.ipsec_encryption=aes-cbc`：361 条 | Go/Rust 的 IMS 默认安全机制已有 hmac-sha-1-96/aes-cbc；不能与 IKE CHILD_SA 的 ESP 算法混为一谈 | `security_client_mechanisms` 等模板机制已能限定表达 | 已有能力，无需为“融合”重复添加。精确 Security-Server 选择、端口/SPI与 malformed offer 仍遵循现有严格校验。 |
| 3HK `ipsec_mode=optional`、wLan ePDG；45403/45404 + ICCID 8985203* | wLan 地址已有；Rust `SecAgreeMode::Auto` 在 401/407 无 Security-Server 时允许 plain，收到有效 offer 时进入安全路径 | `ims_register_template.sec_agree_mode=auto` 已可等价表达 optional；不需要新枚举或全局 `disabled` | 现有 Auto 模型可以表达 optional；真实所选模板及运营商握手仍待验收。本轮没有在真实 3HK 验收；只需在后续精确 profile 集成时复核无 offer、有 offer、畸形/降级攻击、重注册四类用例，不把畸形头当“缺省可跳过”。 |
| giffgaff `transport=tcp`、`user_agent=iOS/18.6.2 iPhone`、GSMA、mid-call/smsip；23410 + GID1 508* | `DefaultGiffgaffTemplate`、`readerIMSUserAgent` 与 Rust `ims_profile.rs` 已专门实现 TCP、GSMA、iOS18.6.2、header/contact 顺序及安全机制 | TCP、GSMA、已有 contact tag 顺序可表达；User-Agent 为已实现的精确 carrier preset | 已覆盖，4 个定向 Rust 测试通过。保持精确 GID1 选择；不能把所有 O2 23410 都当 giffgaff，也不直接追加较宽 SPN-only 规则。 |
| giffgaff/3HK `pani_enabled=true, pani_country=AUTO` | 当前 giffgaff PANI 固定 GB + node-id；Rust builder 也有固定 GB 分支。与 AUTO 不完全相同 | 有是否带 PANI/access_type，**没有**等价 `pani_country=AUTO` 字段 | 真正差异进入 P3：定义可信国家输入与未知值回退、隐私/日志约束、首次/认证后 REGISTER 一致性；按精确 profile 选择。需 GB/非GB/未知来源/更新后重注册 fixture，不全 PLMN 修改。 |
| RedPocket `identity_profile=att`, `register_profile=att`；310170/410/280 + GID1 42 | 当前精确 GID selector 与 ATT ePDG 已有；无这些 symbolic preset 的完整运行时选择层 | realm/domain、identity source、有限 header 可配置，但不能把几个字符串当完整 ATT 行为等价替换 | P3 增加有类型、精确 GID/PLMN 作用域的 ATT identity/register preset；验证 AKA 用户名、IMPI/IMPU、REGISTER/Auth/Security、SMS URI。SPN-only 扩围另行证明，不能覆盖同 PLMN 全部卡。 |
| Ultra/Mint `route=310240`、mnc240 ePDG、smsip-msisdnless 与 smsip-msisdn-less、accesstype=wlan1；310240/260 + 六组 GID | 当前已有对应 GID 子模板，首选多为 `ss.epdg...mnc260`，12 个观察点与参考冲突 | `contact_param_order` 仅允许已知 tag，未容纳两种 MSISDNless tag；PLMN override 不能精确区分各 GID | P3 typed contact capability 列表 + 受控 route preset；先证明 mnc240/260 的归属，再验证所有六组GID、非Mint同PLMN、双拼写注册、独立 SMS/重注册，不抢改现有默认。 |
| CTExcel `route=23433`、`dial_uri_scheme=sip`、`user_eq_phone=true`；23430/33 + 指定 SPN/ICCID | 23433 标准 ePDG与已有 CTExcel call/IMS policy 均保留；23430 借宿身份切 route 不是仅替换一个域名 | 部分域名/phone_context可表达，REGISTER override 不涵盖全套 dial/SMS URI 策略 | P3 将原始身份、route身份、SIP `;user=phone`、语音/SMS目的 URI 策略分别建模；23430与23433、非CTExcel同PLMN、国际号码/短码/普通SIP URI做差分，不能仅换ePDG。 |
| DITO `subscriber_imsi_rewrite` 204047616*→515661015*、route51566、legacy方案；限定 ICCID/SPN或 sponsorIMSI+ICCID | 当前没有可直接复用的 rewrite能力；51566 与515066 保持不同 selector输入，均仅标准 fallback | 没有安全等价 override；直接写新 ePDG 不能完成 AKA/IMS 身份绑定 | P3 保留不可变真实SIM身份与派生展示身份，限定原始IMSI+ICCID+home条件及凭据所有者，验证 EAP/IMS 一致、后缀/长度/反例、无敏感日志，不用字符串全局替换。运营商与订阅授权、兼容性需实卡验收。 |
| 1GLOBAL/Truphone 固定 GID1 与新增90128/90143、全局品牌 SPN | 现有精确 GID 路径已存在；原 bundle SupportedSIMs 支持特定 home PLMN+GID，不能证明全局SPN/新901范围 | 地址可覆写；全局SPN不是当前PLMN override可安全表达的身份范围 | P3 区分 home身份与漫游PLMN；为新范围取得明确来源并加负例，同品牌但不匹配GID/ICCID不得抢占。合成90128仍为标准 fallback。 |
| IP stack、DPD、NAT keepalive、lifetime、完整 ESP proposal/EAP template | VoCat当前 JSON 中**无这些每运营商字段**；Switch数据与现有override已包含更完整的这些参数 | `ip_stack`、`dpd_interval`、`nat_keepalive`、proposal等已有；EAP模板来自受限profile，不能任意注入 | 本轮没有所谓“VoCat每国DPD/IP方案”可导入。未知身份匹配字段使审计失败；新增配置字段列入清单后需人工审阅，再扩展固定来源审计schema。现有保持不动。 |

对应代码：`backend/internal/vowifi/carrier/overrides.go`、`profile.go`、`embedded_registry.go`；`backend/internal/vowifi/policy/defaults.go`；`backend/cmd/proletarius-macos/reader_vowifi_effective_config.go`、`reader_vowifi_session.go`；`rust/crates/proletarius-vowifi/src/{carrier_runtime_profile,ims_profile}.rs`；`rust/crates/proletarius-ims/src/lib.rs`。本轮没有改动这些生产逻辑。

## 离线复跑与实际验证

准备目录为 `Switch/build/research/vocat-carriers-20261005`，包含上述两个清单及对应 VoCat 文件、`ios-carrier-profiles/` 下 MIT YAML；不执行下载下来的参考代码。审计脚本校验 SHA/revision，使用 Ruby `YAML.safe_load` 解析数据，只生成合成身份与报告。

```sh
cd /Users/proletarius/Desktop/Switch/src
export SWITCH_SOURCE_ROOT="$PWD"
. scripts/workspace-env.sh
python3 scripts/audit-vocat-carriers.py
cargo +1.98.0 run --manifest-path rust/Cargo.toml --locked -p proletarius-vowifi --example carrier_replay -- /Users/proletarius/Desktop/Switch/build/research/vocat-carriers-20261005/audit/rust-replay.json /Users/proletarius/Desktop/Switch/build/research/vocat-carriers-20261005/audit/synthetic-cases.json
python3 scripts/audit-vocat-carriers.py --replay /Users/proletarius/Desktop/Switch/build/research/vocat-carriers-20261005/audit/rust-replay.json
python3 -m unittest discover -s scripts/tests -p test_audit_vocat_carriers.py -v
cargo +1.98.0 test --manifest-path rust/Cargo.toml --locked -p proletarius-vowifi --test carrier_reference
cargo +1.98.0 test --manifest-path rust/Cargo.toml --locked -p proletarius-vowifi giffgaff
cargo +1.98.0 run --manifest-path rust/Cargo.toml --locked -p proletarius-vowifi --example carrier_replay -- /Users/proletarius/Desktop/Switch/build/research/vocat-carriers-20261005/audit/standard-rust-replay.json
cd backend
PROLETARIUS_CARRIER_REPLAY=/Users/proletarius/Desktop/Switch/build/research/vocat-carriers-20261005/audit/standard-rust-replay.json go test ./internal/vowifi/carrier ./internal/vowifi/policy
```

本轮结果：Python 7 测通过；Go/Rust共享25个边界案例通过；giffgaff相关4个Rust测试通过；原2722例全参数Rust→Go差分通过；carrier/policy包测试通过。2546例新审计使用实际 Rust resolver，不在脚本中重写选择器规则。原有与新增离线验证均不能证明真实网络可达、订阅开放 VoWiFi、长期在线或运营商准入。

实施顺序：已覆盖项停止重复导入；P3先实现精确scope/preset与有类型的header能力，再处理PANI国家输入、ATT、Ultra/Mint和CTExcel，最后处理需要身份转换的DITO及尚无归属证据的新增home selector。每项完成固定fixture、Go/Rust参数差分、loopback协议回放及经授权的真实卡验收后，才提升支持状态。
