# Iteration 618：P6当前候选版本原位往返验收

日期：2026-10-21。范围：修正P6隔离测试脚本对候选版本的硬编码，并用当前冷构建候选执行一次复制Bundle往返。全程只使用合成数据、复制出的应用Bundle和硬件/网络拒绝沙箱；没有读写真实用户数据、连接读卡器、发送短信、拨号或生成DMG。

## 工具修正

`scripts/check-p6-inplace-bundle-roundtrip.py` 原先将候选版本固定为3.43，导致它不能验证当前3.46候选。现在增加 `--expected-candidate-version`，按 `CFBundleShortVersionString` 做精确核对，默认仍为3.43以兼容历史复现命令。新增命令行回归覆盖历史默认值和显式3.46参数。

## 当前候选的合成验证

执行结果见 [`iteration-618-p6-current-cold-candidate-roundtrip-final.json`](../../../../build/reports/iteration-618-p6-current-cold-candidate-roundtrip-final.json)。脚本分别复制3.42/build342旧Bundle与3.46/build346当前冷构建Bundle；候选来源为Iteration617冷构建，所嵌源码指纹为 `d0d71f9df48d7e0f450bcb2ec34d618f442d612de0b8a868bcbda3154fa1af9b`。输入合成业务数据在每代间保持一致，原始fixture哈希保持不变。

- 清洁根控制通过。
- 3.42旧版首次写入及3.46原位升级后，数据往返断言通过。
- 两次3.46启动均检测到旧版遗留的模块恢复状态，正确保持隔离、不改写恢复证据；daemon均在15秒内退出0、写出Rust shutdown receipt，并移除自有socket。
- 3.42首次运行与回退仍各以退出码1结束，日志均为 `call_mode_shutdown_unconfirmed`；它没有输出native shutdown receipt。原结果保留，未按新版的安全隔离行为覆盖旧失败。
- 因旧版两次非零退出，整个原位往返脚本结论为**失败**。数据可读写与新版安全隔离通过，不等于完整升级/回退通过，也不证明实机模块路由清理成功。

完整Python脚本测试174项通过；`check-rust-migration-contracts.py --require-rust-only --diff`确认294条HTTP契约归Rust、0条归Go；验收台账元数据一致，但正式分母仍未冻结。`clean-generated.rb --list`未发现需要清理的构建缓存。

## 当前阶段与交付门槛

P0仍未闭合，P1–P4实机/业务验收仍在推进，P5真实通话听音待用户可配合，P6升级回退未通过。正式叶子台账为0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；分母未冻结，故不报告整体百分比。接口归属294/294仅代表代码所有权。三轮独立交叉审议尚未开始；须待全部代码和适用功能验收完成后，逐轮修复并复测发现项，最终发行门禁通过后才生成DMG。

本轮修改仅涉及P6验收工具、它的CLI回归测试和进度文档；当前3.46 `.app` 是前一源码指纹的候选，不是本轮修改后的最终发行候选。最终出包前必须从冻结的最终源码重新完成完整构建、签名、包内与运行验收。
