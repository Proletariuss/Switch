# Iteration 309：强制 N待 reservation 绑定 reader 身份

reader reservation 的 owner token 此前可以在缺少 reader 身份时建立 owner；Rust handler 通过 `Value::as_str/as_u64` 读取字段，错误类型在两个字段同时存在时可能折叠成无身份请求。Go client 也允许 acquire 调用方省略绑定，reader 名称长度没有和 Rust 请求上限对齐。

现在 Rust acquire 必须收到 `reader` 字符串与 `reader_generation` 非零无符号整数；名称不得为空白且最长 512 字节。缺字段、类型错误、零代次、空名称或超长名称均返回 400。owner-token retry 仍要求绑定与当前 owner 完全一致；release 继续按 token 处理，以保留失败清理路径。Go client 对 acquire 强制提供唯一完整绑定，并在发送前执行相同范围检查。Go 的 inventory 和 Profile acquire 调用均带当前 reader 与 generation。

验证：Rust `reader_reservation::tests` 5 项通过；Go reservation 定向测试和 `cmd/proletarius-macos` 全包测试通过；完整 `scripts/test-migration-gates.sh` 通过（Rust workspace、Go vet/全包测试及现有跨语言 parity）。迁移契约扫描通过，清单为 334 个 method/path contract（Rust 273、Go 61）、127 个 Swift 调用点和 327 个审阅范围。

这是 reservation 输入/身份约束加固，不是 reader actor 或所有 Go APDU 消费者迁移。Rust gate 仍为 daemon 进程内门闩，Go 仍持有 `authMu`、reader lease lifecycle、硬件操作及 N待会话执行；总体进度保持 31.4%（约 ±3 个百分点）。未运行真实读卡、eSIM 写入、短信/通话或运营商测试；未生成 DMG。
