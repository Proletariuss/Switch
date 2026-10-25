# Iteration596：入口映射、范围规则与基线恢复

本轮处理P0验收资料和校验工具，不修改Rust、Swift、codec或其他生产业务实现，不重新编译App。用户暂缓入站短信；本轮没有硬件、运营商、短信或呼叫测试，没有安装或生成DMG。

## 已写入源码

- [入口映射](entry-acceptance-map.json)：对应既有294条API、131个Swift调用点、56条后台能力。原始目录身份、派生HEAD和条件业务归属保留；未证实的精确handler、磁盘/DTO格式和实际退出链保持显式未知。
- [范围协议](acceptance-scope-protocol.json)：21类有限协议规则及10项政策决议。5项仍待历史支持成员事实，另5项规则已定义、执行待验。未增加需求ID、功能PASS或支持实例。
- [基线来源](baseline-provenance.json)：分开记录seed、2.80前后、迁移切换前及当前切点，避免把不同时期的资料互相替代。
- [台账](acceptance-ledger-2026-10-19.json)和[校验工具](../../../scripts/check-acceptance-ledger.py)：保留原115个叶子的必需判据；校验入口身份、条件适用性、源指纹及虚构完成率等错误。

原MultiStandby调用注释引用了不存在的`LiveMultiStandbyAPI`，现更正为默认`MultiStandbyHTTPAPI`经既有POST路径发送。可注入的协议实现仍保留，不把模型调用增加为一个直接API调用点。旁边match arm的函数已从规范映射中移除，不能据此认定handler归属。

## 实际恢复及校验范围

2.63 seed已在隔离的`build/baselines/iteration-596-seed-263-recovered`实际恢复：3086个固定Git commit blob，加1个原清单SHA完全匹配的文件，共3087文件、46,067,337字节。恢复后逐项校验完整路径集合、大小和SHA；原清单和补充文件保持不变。执行证据为`build/reports/iteration-596-seed-recovery-result.json`。

这只证明seed清单内的内容恢复。2.80及实际迁移切换前的完整字节链、dirty/untracked差异和环境资料仍未建立；有限有出处locator未找到不表示全机不存在。未将seed恢复记作两个历史基线叶子PASS。

实际v3入口/台账元数据校验通过，28项防误计数测试通过；其中27项为缺条目、重复身份、越界场景、虚构PASS和基线混用等负向修改，另1项核对当前待验状态。API普查仍为294 Rust、0 Go、54派生HEAD和470个带指纹审阅范围。最终`--require-frozen`门禁实际退出1，保持未通过；当前没有已审定的完整实例登记，不能发布N或整体百分比。

首次v1元数据校验退出1：新工具未列入原有5项`CANDIDATE_SOFTWARE_SUBSET`证据状态。原失败报告和脚本均保留；v2仅补入这个既有facet状态，要求有证据引用且叶子仍为PARTIAL，不改成PASS。随后静审发现“有效但指向另一场景/政策”的引用也应拒绝，v3补上严格身份校验及两项负向用例；当前实际38个引用本来就正确，未改需求或业务。完整结果及最终源指纹见`build/reports/iteration-596-final-review.json`；更早的无v3后缀source快照是中间工具版本，不作为最终切点。

上一工作包595的完整daemon3022项、有限I/O与进程恢复软件结果保留在各自源码/二进制范围内。本轮不会把它们改写成596新运行结果；HTTP仍是92项执行断言通过、2项year1 OS未验证，原aggregate false保留。生产源未变时无需重复运行全套耗时回归。

## 整体状态

接口接管100%（294/294，含54条派生HEAD）；P0未闭合，P1–P4验收中、P5完善中、P6未完成。仍需完整producer/writer/exit及旧数据格式映射、历史支持成员与实例登记、当前交付版本实机业务、自然租期长稳、受控整机性能、完整升级回退及干净环境验收。入站短信本次暂缓。
