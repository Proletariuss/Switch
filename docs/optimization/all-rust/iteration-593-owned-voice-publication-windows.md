# Iteration593：原 Managed 语音与双目录进程退出软件验收

2026-10-20。实际完整daemon 3003项通过，保留592全部2995个通过名称，新增8项；真实原Owned记录的Running/InitiallyExited清理链、14个断言场景以及34个定义的双目录进程退出窗口和8个受控负向场景通过。这些是inert byte-I/O软件证据，不是实机恢复或断电/任意OS写入与fsync故障验收。HTTP固定94项为92项执行断言通过、2项year1未验证，原aggregate false/exit1保留。现有backup create/import/delete完整生命周期、真实USB/运营商和P0–P6完整验收仍待完成；无update/replace API，本轮无硬件/SMS/call、未安装、无DMG。接口294/294的100%仅为归属覆盖；P1–P4同步验收、P5完善、P6未完成，无整体百分比。

生产原Device opener/claim和精确SET/GET后进入同一私有consuming executor；测试以真正ManagedControl::create产生随机session/key和原record，原writer FD结束后重新取得实际record pin。签名peer的私有Zeroizing key不导出、不出现在诊断。真实AdbProbe framing、随机nonce、HMAC、原boot/PID/start/argv、STATUS→STOP→STATUS(必要时)→DSP/双PCM→RELEASE、checked ADB close与record-pin release共享生产代码。取消或ledger篡改后只准完成已Prepare的固定清理链，不能生成配置写许可或receipt。STOP/RELEASE ACK丢失保留Pending，不重放旧Unknown；原journal、control字节和inode在两个目录的retirement后保留。正向原Running、InitiallyExited及12个负向场景均由实际完整测试和8项focused/14场景的外层到达断言绑定，不以caught panic冒充晚期分支覆盖。

实际2992输入，相对592/2989为3新增、10已有变更，union差异13；完整daemon 3003 PASS、0 FAIL、4 ignored，保留原2995并新增8。4个ignored辅助测试没有记入PASS；原journal的8场景/16子进程与新publication的34定义窗口+8负向/84子进程分别由专用驱动实际执行。新worker只在真实Some原voice→full native byte-I/O→join后在生产finalizer边界exit86；独立新进程取得fresh Lease/Gate，经metadata续接、原key HMAC、两目录raw/inode及prefix/exactly-once receipt核验，再确认第二个lifetime。零计数只覆盖production reconciliation和shared session两个入口，不能声称全进程全部native factory从未尝试。

34窗口为每目录14个Prepare/rename/父目录sync/Placement边界，加manifest/publication/receipt前后6处。8负向仅在各目录before-prepare注入both-present、both-absent、foreign-destination、tamper；不代表所有阶段交叉组合。process::exit仅验证进程退出后的继续性，不验证底层记录torn-write、OS write/fsync故障、设备断电或所有retirement故障。all_retirement_crash_windows_verified与all_os_write_fsync_or_power_loss_verified仍false。

24项规定软件报告、原8/16门、genuine bound-main Requested1/Pending门与新42/84 publication矩阵均须原报告真实通过才发布。bound-main在硬件/网络拒绝沙箱内保留原Unknown1，不能当作USB恢复成功。HTTP完全复用590 fixed94矩阵及其原SHA，92执行断言通过、2year1因OS拒绝/钳制未验证，原passed=false/command exit1保留并排除于24项软件通过。GUI与Swift工具复用未改的581原物；本轮Rust编译与Release/package归各自实际报告。

最终review builder首次因把matrix外层passed/run包装误当raw run而在canonical写入前KeyError退出1；原工具输出转录与log、before脚本和窄修diff保留。该转录不冒充run_owned PID/group证据，也不是源码或测试失败；实际门禁报告不改，修正后的builder重新读真实报告才产生当前review。

592原162项证据及既往失败cut、原日志、Products、runtime、baseline、正式dist、已安装app、EVS/ARM资源均按原hash保留；规范plan与leaf ledger独立pin不改。只精确清理3项可重建缓存，分配字节1809289216和同期可用空间变化1598353408分别记录。未生成DMG或安装candidate。

正常恢复后的现有backup create/import/delete完整生命周期仍未完成；没有update/replace API，不能把不存在的接口列为已完成验收。真实USB/ADB写入、重启、原voice清理与receipt/Gate实机验收、RELEASE ACK丢失无远端tombstone、voice-only/legacy/global仲裁、运营商短信/通话、自然期限与24h/72h长稳、升级回滚、性能及完整P0–P6仍待完成。历史589注册仅属原版本；本轮无新硬件验收，入站短信继续延期。

证据：[完整汇总](../../../../build/reports/iteration-593-gate-summary.json)、[完整软件](../../../../build/reports/iteration-593-final-v1-software-result.json)、[名称差集](../../../../build/reports/iteration-593-final-v1-daemon-test-census.json)、[Owned focused](../../../../build/reports/iteration-593-integration-v1-owned-focused.json)、[34+8窗口](../../../../build/reports/iteration-593-publication-crash.json)、[实际生产审议](../../../../build/reports/iteration-593-final-production-review.json)、[bound来源](../../../../build/reports/iteration-593-final-v1-bound-journal-fixture-provenance.json)、[HTTP原false](../../../../build/reports/iteration-593-http-final-result.json)、[精确清理](../../../../build/reports/iteration-593-cache-cleanup.json)、[592原汇总](../../../../build/reports/iteration-592-final-v2-gate-summary.json)。
