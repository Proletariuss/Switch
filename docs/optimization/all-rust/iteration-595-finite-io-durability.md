# Iteration595：有限I/O故障与元数据耐久重验

2026-10-20（UTC+8）。

实际完整daemon 3022项通过，保留594全部3022个通过名称，新增普通通过0项；8个忽略项为专用进程helper，不计通过。本轮仅在严格typed scan/coverage之后，对真实固定FD的完整raw/stamp/placement进行前后校验并执行record、payload及parent/root同步；读口失败保留Pending，不改写原raw/inode、不重放native。有限I/O错误返回13场景/26子进程通过：1短写控制、7 Pending、5 metadata recovered；B01/B02/B03由真实delegated record/parent/root sync ACK验收。保留原v1 source cut与testartifact，以仅两reviewJSON差异精确绑定最终cut，未重复或改写旧PASS。当前595发布进程退出42场景/84子进程（34定义窗口+8受控负向）和备份105场景/210子进程（27 Pending、5 no-op、10 aborted、63 committed）分别通过；旧594全部416报告独立保留。正常v2/v3 create/import/delete、真实RootProof/库存锁/私有归档和100碰撞规则保持，无 update/replace API。生命周期日志达到上限后拒绝新写，尚未实现日志压缩。三个Release main进程与genuine boundUnknown通过。HTTP固定94为92执行通过+2 year1跳过未验证，原aggregate false/exit1保留，排除于24软件必需门禁。实际清理2项Ruby精确可重建缓存，回收分配空间1792167936字节。接口294/294、100%仅为归属覆盖；P1–P4同步验收、P5完善、P6未完成，无整体百分比。P0候选子证据不是规范台账变更。voice-control-only恢复、deploy/calibration义务、完整P0–P6及真实USB/运营商/长稳仍待验收；用户暂缓入站短信。有限I/O返回错误与进程退出不等于所有OS write/fsync/断电窗口。本轮无硬件/SMS/call、未安装、无DMG。

冻结输入3000（原2997）；人工生产审阅4文件/3更新scope+1新增durability whole-file，共470scopes。

原594失败cut、辅助panic反例、无效ABI探针和mode000元数据例外全部保留；仅原500个空文件使用既有完整stat例外，不读内容、不改mode、不伪造rawSHA。其他读取/权限错误均失败。

证据：[最终生产审阅](../../../../build/reports/iteration-595-final-production-review.json)、[门禁汇总](../../../../build/reports/iteration-595-final-v1-gate-summary.json)、[有限I/O原v1](../../../../build/reports/iteration-595-finite-io-v1.json)、[缓存清理](../../../../build/reports/iteration-595-final-v1-cache-cleanup.json)。规范计划/ledger、正式dist、已安装应用、离线依赖、EVS/ARM、所有Products与原始证据保留。
