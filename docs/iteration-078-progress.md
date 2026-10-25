# Iteration 078：复用 Celmux SIP transaction runtime

日期：2026-10-05。实现与记录仅写入 `/Users/proletarius/Desktop/Switch/src`；参考目录 `/Users/proletarius/Downloads/celmux-src/` 未改动。

## 结果

参考代码复核确认，Switch 已复用 Celmux 的 MIT CCID/eUICC/LPA、QMI wire/client 基础和 SIP parser/dialog/transaction primitives；AT 命令/URC session 则只做过行为适配，维持 Switch 唯一串口 actor。此次补上之前未采用的 Celmux `crates/sip/runtime/src/lib.rs`，作为独立的 `proletarius-sip-runtime` workspace crate：有界管理 client/server SIP 事务与定时器，向调用方交付重传/发送动作；它自身不建线程、不打开 socket、不改变 Swift/HTTP API。

适配仅改本地核心 crate 的 import 名称。来源文件 SHA-256 为 `827e9c67e8821881d9dd7dc7dbf807b5766e3a125e559a12f4956b905b177044`。源文件头标 `SPDX-License-Identifier: MIT`，Celmux 对应 crate manifest 写 BSD-2-Clause；本次按实际被采用文件的 SPDX 保留 MIT 源码头，并已把来源及 MIT 条款登记在 `THIRD_PARTY_NOTICES.md`。快照没有 `.git` 元数据，无法核实 commit 或分支。没有复制 Celmux IMS/VoWiFi application/session runtime、平台网络传输、读卡器 host 或 UI。

新增单元测试覆盖定时器更新去重、INVITE retransmit 与 provisional response、容量拒绝不留半成品状态，以及关闭后的资源释放/拒绝新工作。

## 验证

- `cargo +1.98 test --offline -p proletarius-sip-runtime`：4 项通过。
- `cargo +1.98 test --workspace --offline`：全 workspace 通过；`proletariusd` 348 项通过。两条既有 `native_vowifi_session.rs` dead-code warnings 与本次改动无关。
- `cargo +1.98 fmt --all -- --check`、`git diff --check` 通过。
- `python3 scripts/check-rust-migration-contracts.py` 通过：195 项 method/path contract、Rust 92、Go 103、127 个 Swift HTTP 调用点及 69 个审阅指纹与源码一致；本轮没有改变 HTTP owner。

## 迁移进度

全 Rust 改造进度仍是 **21.1%（10.0 / 47.5 人周等值，误差约 ±3 个百分点）**。该 crate 尚未进入生产 IMS dispatcher，亦未差分真实 Go SIP 会话轨迹；因此它只减少后续 P5 通用事务层的重复编码，不提高端到端迁移完成率，也不按 406 行直接扣减工期。P2B 仍是当前阶段，继续完成 Go 硬件调用及 eSIM/LPA owner 收口后，再把此 runtime 用于 P5 并以 Go 轨迹验证。

没有生成 DMG、没有启动应用、没有连接读卡器或蜂窝设备，也没有处理 9eSIM `6982`。
