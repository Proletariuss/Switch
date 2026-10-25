# Iteration 304：AT 诊断失败映射收归 Rust

公开 `POST /api/at` 已由 Rust 校验命令、执行 RF/VoWiFi 准入并分类设备响应，但 Go 内部桥此前仍将底层错误转换成面向用户的 HTTP 状态码、错误码和中文提示。本轮将桥协议收窄为原始 `response` 与底层 `error` 文本；Rust 负责映射飞行模式冲突、VoWiFi 通道占用及传输失败，成功/模块 `ERROR` 响应仍由 Rust 投影。Go 继续持有跨业务 SIM/APDU 互斥锁，并把物理命令交给 Rust modem actor；未更改锁、命令或硬件执行路径。

验证：Rust AT console 6 项及 Go 内部桥专项通过；完整 `scripts/test-migration-gates.sh` 通过（Rust workspace **1194 passed / 3 ignored**，Go vet/全包测试及现有载波、USB、Radio、HTTP、SDP、AMR、MESSAGE parity 均通过）。Go 1.26.3自动工具链下载超时，使用本机 Go 1.26.7 经 Switch/build 临时 shim 验证；shim 已清理。格式检查与 `git diff --check` 通过。

本轮把 AT 诊断的公开错误语义从 Go 移到 Rust，但 Go 的SIM/APDU互斥与执行桥仍是运行依赖；串口 actor持有、无线电准入、实机差分和 Go退出尚未满足，因此不增加加权迁移进度，仍为 **31.4%**。未访问读卡器/实体卡、未发短信、未拨号、未生成 DMG。
