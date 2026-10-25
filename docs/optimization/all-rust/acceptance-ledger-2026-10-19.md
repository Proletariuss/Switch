# P0–P6 验收台账：当前基线范围已采纳，整体分母未冻结

2026-10-23，P0-1 已按用户同意的“当前可验证基线”正式替换两个历史需求叶子：`P0-1.working-baseline` → `P0-1.current-source-baseline`，`P0-1.baseline-280` → `P0-1.current-build-baseline`。原历史判据与未通过状态保留在 [i677 归档](acceptance-ledger-2026-10-19-iteration-677.json)和[激活前归档](acceptance-ledger-2026-10-19-iteration-680-pre-activation.json)，[激活决定](p0-1-current-baseline-activation-2026-10-23.json)记录一对一映射。其他113个需求叶子未变；现行 [JSON 台账](acceptance-ledger-2026-10-19.json)仍为49个父分类、115个需求叶子、21类场景，0 PASS / 5 PARTIAL / 110 NOT_REVIEWED。115只是目录条数，不是正式验收分母；分母未冻结，不计算整体百分比。旧2.80、实际迁移切换前工作区及旧版等价性均未因此通过。

2026-10-25当前只读源码审计为312条HTTP method/path（Rust312/Go0，含59条HEAD别名）、130个Swift HTTP调用点、481个审阅scope，见[当前运行时所有权](README.md#current-runtime-ownership)和[G7证据](../../../../build/reports/stability-implementation-20261025/g7-current-docs-implementation/IMPLEMENTATION.md)。现有[入口映射](entry-acceptance-map.json)仍保留其304条HTTP、127个Swift调用点、56条后台能力和57条HEAD的旧映射切点；它尚未被批准为当前源码分母。本轮不刷新映射、库存、指纹或叶子状态，来源/库存审计exit1及原始失败保留，G0B待人工审定。HEAD派生入口链接原GET，不重复计业务完成率。`3.48/build348` 的源码恢复和隔离冷构建收据仅适用于该版本；后续交付目标版本需另核同源收据。软件验收不替代洁净安装、设备、运营商、实体音频、外部投递、长稳及旧版回退验收。当前唯一工作区是 `/Users/StatTrak/Desktop/Switch_MAC/`，源码、构建和发行物分别在 `src/`、`build/`、`dist/`。

[适用范围协议](acceptance-scope-protocol.json)载明当前P0-1判据及10项政策的具体规则；设备/固件、macOS QMI/MBIM、线路业务、升级版本及旧owner成员仍须补齐事实。聚合入口按实际行为分支关联既有叶子；传输或调用上下文不自动扩展叶子的适用范围。代码附近的函数不能当作精确handler，逻辑归属、精确数据格式和完整退出链分别记录。仍为UNKNOWN的项不会因映射文件存在而通过验收。

## 历史证据与验收规则

[历史基线登记](baseline-provenance.json)区分2.63 seed、pre2.80、final2.80、迁移切换前及当前已验证切点。2.63 seed的3087个文件已恢复并逐项校验内容；它不能替代2.80或真实迁移切换前的dirty/untracked工作区，原历史所有权、时间、ACL和扩展属性也未据此证明。未定位的确切归档及缺失增量继续登记。

历史工作包597新增[有限归属/格式/退出清单](runtime-owner-format-exit-map.json)与实际转发退出/配置修复；当时343个锚点已逐项校验，14个旧锚点标为superseded或待定位。当时软件回归3036项通过，不授整个需求叶子PASS。用户确认2.80及转Rust前完整归档已不再保留，按“历史资料不可用”记录且不再反复索要；这不是原等价性验收的通过证明。

以下规则已固定为后续验收约束，尚不代表已通过：

- 主机保持EVS Primary优先、独立AMR-WB后备，不使用AMR-NB/PCMA。能力报告、SDP广告、接纳和实际codec路径必须一致；本轮不改变运行开关。未来若启用EVS RTP IO切换，需有对应真实编码、解码和切换证据。
- 长稳阶段至少24小时，最终至少72小时且至少3次自然租期事件；IMS、EAP、IKE和CHILD分别记录覆盖，虚拟时钟或故障注入不代验自然事件。
- 采用原计划§6.2的整机性能预算，比较完整进程组，固定Release、机器、网络代理、数据量和采样脚本；噪声无法辨别时保持INCONCLUSIVE。
- 干净构建与干净安装分别验收。同机空输出目录可以证明有限构建闭包，不能证明Keychain、权限及旧用户环境已清洁。
- normal恢复与startup reconciliation分别验收。Unknown不自动重放；缺失原绑定保持Pending，真实同步、关闭、释放和独立receipt分别提供证据。

唯一计分身份采用“既有叶子×既有场景类×已审定语义实例”。API别名、重复调用、测试条数、时间戳和构建hash不产生新单元；normal/cancel/partial/durable/release只合取本叶子REQUIRED栏。NOT_APPLICABLE须有原要求支持的具体理由，不能隐藏缺卡、未测或失败。

运行以下校验可检查目录身份、源文件指纹、条件归属、未知项及禁止虚构PASS等记录约束。它不启动App或访问设备，成功只表示元数据一致：

```sh
python3 -B scripts/check-acceptance-ledger.py
python3 -B -m unittest discover -s scripts/tests -p test_acceptance_ledger.py
```

最终门禁使用`--require-frozen`；当前应失败。完整语义实例和证据登记尚未建立，工具拒绝以115条叶子或接口数替代N。后续范围变更须版本化旧新映射，历史失败和不同二进制的成功记录均不改写或自动转移。
