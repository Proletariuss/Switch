# Iteration 073：Celmux NAS 运营商名称查询适配

日期：2026-10-05。全 Rust 后端持续目标下的 P2B/QMI 工作。所有修改均在 `/Users/proletarius/Desktop/Switch/src`；`/Users/proletarius/Downloads/celmux-src` 为只读参考。

## 完成内容

- 新增 QMI NAS service ID `0x03`、Get Operator Name message `0x0039` 与 `NasClient`。
- 响应需匹配 NAS service/message 和 QMI response flag，并通过 result TLV 校验。
- 按 Go 当前解析规则读取 TLV `0x10` 的 provider name（丢弃首字节）及 TLV `0x13` 的 operator string；选择名称时 provider name 优先、空值才回退 operator string。
- 只适配 Go QMI enhancement 当前实际使用的 query，不引入 Celmux 完整 NAS scan/signal/network-register codec 或 manager。
- 复用对象：Cel mux MIT QMI service 层 `crates/qmi/services/src/nas.rs`；参考快照无 Git metadata，以 SHA-256 `a02dc34827e732820b69be619bfdb201fd16ddbff5cb9cdfcafeb36357141f96` 固定。

## 验证与限制

- `proletarius-qmi-services` 定向测试现为 11 项通过：Go IMS codec 向量 7 项、NAS 名称选择 1 项、IMS/IMSA/NAS client lifecycle 3 项。
- NAS 生命周期通过 fake transport 验证 client-ID allocation、message/service/client-ID、SPN/operator-name 解析与 close 释放。
- `bash scripts/test-migration-gates.sh` 通过：Rust workspace、`switchd-rs` 336 项、Go vet/全包测试、2,722 个 carrier replay 及 PCMA/SDP/AMR parity 均成功；现有两个 `native_vowifi_session.rs` dead-code warning 和 Go 链接器重复 `-lusb-1.0` warning 未影响退出码 0。
- Go 的 `GetNativeMCCMNC` 读的是 home PLMN：先查 QMI UIM 的 EF-OPL，再回退 IMSI/EF-AD；它不能用 NAS 当前 serving MCC/MNC 近似替代。Rust 等价 UIM/APDU OPL 数据路径与 owner 尚未接通。
- `NasClient` 尚未接入 `switchd-rs`，没有打开物理 QMI 设备或验证模块响应。未进行卡片修改、短信发送或 DMG 构建。

## 进度

编解码与 service wrapper 仍不等于设备 owner 接管；P2B 保持 **22%**，整体保持 **20.0% 已完成 / 80.0% 待完成**（约 ±3 个百分点）。后续按 Go 可达行为完成 home MCC/MNC 读取、受选后端的唯一 QMI owner、服务失败后的 rebind/replay 与断连关闭，然后再接入 Rust daemon。
