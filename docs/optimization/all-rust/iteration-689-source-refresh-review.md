# Iteration 689 — 历史 P0 证据路径与来源 pin 复核

i688 的 549 条选择性来源快照、九源 57 锚点 QMI 静态 trace 和审阅记录保留原始字节，作为不可变直接前驱。i688 隔离冷胶囊在历史 i677 发行清单的原始路径 pin 测试失败，未产出 App。修复将证据声明恢复为 i677 原始 `dist/release-manifest.json` 路径和 SHA-256；全量历史核对仍从 `dist/archive/Switch-3.48-macOS27-manifest.json` 读取、重算相同字节哈希，并验证清单中的 3.48/build348 来源和 DMG 绑定。来源端与归档端的路径角色分别明确，不修改历史收据。

i689 选择性来源仍为 549 条：相对 i688 仅历史继任核对器及其测试、正式账本核对器及其测试四项改变，其他 545 项 SHA-256 保持。`progress-current.md` 位于清单外，由正式账本独立 pin。QMI 九个源文件及 57 个精确锚点未改；private `.in` 鉴权和 public/`.rs` internal-route 拒绝结论不变。304 个 HTTP 契约的 Rust owner 未变。正式台账保持 49 父项、115 叶、21 场景类、0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；整体分母未冻结，P0–P6 未完成。

历史 P0 三项核对与局部测试只确认修复方向；同源码完整冷验收尚未重新执行。失败的 i688 胶囊及 i685 3.49 软件收据不得授予 i689 3.50 的 App、有限缺陷闭合、产品、设备或运营商 PASS。
