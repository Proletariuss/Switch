# Iteration 564：备注前向兼容与日志阻塞退出

2026-10-18。本轮修复 P0/V6 数据兼容与 P1/RT-H02 退出路径的两个实际缺口，没有把接口接管视作完整产品验收。上一轮缓存清理释放约 2.72 GiB，并完善了只处理明确可再生目录的清理工具。

## 已修改的生产行为

`profile_notes.rs` 原先先把整份文件解析为 `Value`，再转为只有 label/phone/tags 的结构；任意一次保存或记忆号码都会丢弃所有条目的未知字段。现在持久层独立保留原始 JSON 扩展，公开 DTO 和请求仍只有原有字段；公开编辑、内部 remember-phone、注册号码缓存三个 writer 均保留目标与其他条目的扩展。清空已知字段只删除不带扩展的旧条目；有扩展的条目保留扩展和三个空字段，不替用户删除未知数据。

扩展使用 `RawValue`，不先转成浮点值，大于 u64 的整数、高精度小数及嵌套原文也保持。仅 daemon 启用已有 serde_json 的 raw_value feature，没有新增 crate。已知字段仍严格要求字符串；原 Value loader 对重复 object key 的“最后值生效”规则保持，不因采用结构反序列化而改成重复字段报错。原有 8 MiB 文件上限、原子写入、0600 权限及读取/写入失败保留源文件的行为保持。公开接口不返回未知扩展，也不接受客户端写入任意扩展。

`diagnostics.rs` 原先在通信/退出 owner 上同步锁定并写 stderr。现在仅通过容量 64 的队列 try_send；单行最多 4096 bytes，UTF-8 截断有标记。唯一日志线程持有复制的 stderr 文件描述符，不占用 std 全局 stderr 锁；满队列或坏输出丢弃诊断，不阻塞业务。main 四个有最终诊断的返回分支最多等待 100 ms drain，不 join 可能永久阻塞的日志线程。这里只对可丢弃诊断采用此策略，未改变物理设备、发送、持久化或清理回执的等待语义。

## 验证与证据

全部 Rust 命令使用 Rust 1.98.0、locked/offline、显式 sibling build/rust-target，测试关闭 debug symbols 与 incremental。定向结果见 `build/reports/iteration-564-final-groups.json` 及其中各日志：

- profile_notes 组覆盖原有备注、相关模块测试、公开编辑/清空、三 writer、严格类型、超限后源文件不变、原始大数/小数、重复键。
- registration_identity 组通过真实本地 REGISTER fixture 验证号码缓存和其他 Profile 扩展保留；它没有连接运营商。
- diagnostics 与 shutdown 组验证队列满、阻塞 writer、有界 flush、关闭/部分写入错误以及既有退出协调。
- `test_nonblocking_diagnostics.py` 的独立 rustc fixture 覆盖真实 OS 满管道和普通管道，3 项 Python / 5 项 Rust 测试通过；该回归已加入 `test-migration-gates.sh`。

最终四组分别为 23、8、12、9 项，共 52 项通过，0 失败；没有把 standalone 重复诊断单测再加进这个总数。最终受测开发 daemon SHA-256 为 `12c51e73f8338d0cb3680f4f32ed399db34acac6faf792ca010d578b5154a8dd`。

`scripts/check-nonblocking-daemon-shutdown.py` 使用本轮实际构建的 Rust daemon 和全新合成目录。必须先通过 Iteration546 内核 sandbox canary 及子进程继承检查，禁止硬件与外连；没有无沙箱 fallback。测试 available、预先塞满且保持读端打开的 65536-byte 管道、closed 三种 stderr，API 仍可响应，真实 PUT/GET/清空备注保留精确扩展，随后 SIGTERM 正常退出 0、IPC 全部清理。满管道在进程退出后才读取并核对原 sentinel，不把读端关闭或测试强杀计为通过。报告为 `build/reports/iteration-564-daemon-log-backpressure.json`，记录实际 binary SHA、耗时和清理结果；所有合成目录删除。

最终 available/full/closed 退出分别为 0.0038/0.1308/0.0013 s，均没有强杀或子进程/socket 残留。这是合成无硬件环境下的单次退出观测，不是原计划的受控性能 p95 验收。

独立 High 审阅覆盖原始实现、数值精度修正、重复键兼容及进程夹具清理。只更新四个已有生产 review scope；其余 427 个 scope 不变。main 去掉新增四行 flush 后的 SHA 与 Iteration563 冻结 main 完全相同。`iteration-564-source-review.json` 记录旧/新指纹和该证明。294 Rust / 0 Go 路由、127 Swift HTTP 调用点、54 HEAD 与 431 scopes 保持；结构 Go 退出检查及其 20 项负例、契约工具 50 项负例通过。这些门禁仍不替代实机或完整 P6。

最终源码/测试输入 manifest 为 `iteration-564-final-source-snapshot.json`。其中进程夹具按 reviewer 意见补了一处失败后的 owned-child group 清理，校正单独记在 `iteration-564-source-freeze-correction.json`；未改已冻结 Rust 生产输入。最终 drift、缓存清理与实际测试摘要记入 `iteration-564-final-verification.json`。

2850 项 build/test 输入最终零漂移。确认编译进程与 target 打开文件均不存在后，按白名单删除本轮约 1.044 GiB 编译缓存，磁盘可用约 653.5 GiB；保留离线依赖、当前签名应用、回滚基线和报告。受测开发 daemon 随 target 一起删除，可由已记录源码和工具链重建，不将它冒充仍在 dist 的完整应用。

## 完整目标与剩余工作

本轮只构建和验证开发 daemon，没有重建/替换 dist 中 Iteration563 的完整应用、安装应用或生成 DMG；不能把本轮 daemon 测试结果归给旧安装包。满日志管道可能丢诊断，通信/持久化/物理退出结果仍由原机制决定；这个取舍用于避免可选日志控制业务生命周期。

[P5 编码重协商审议](iteration-565-p5-renegotiation-residual.md) 已把后续实现约束和完整转换测试列出，未将“同一 AMR-WB 协商内 CMR”误当作 SDP 重协商完成。生产回退共享生命周期 authority、未知旧版历史与旧3.42退出1仍保持 P6 缺口。真实语音/短信、普通 LPA、BLE 重连、多 Profile N待、自然租期长稳、安装回退和受控性能仍各自待验收，整体目标继续 active，不估算无依据百分比。
