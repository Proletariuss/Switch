# Iteration585：耐久还原journal与真实进程死亡门

整体：接口接管 **100%（294/294）**；P1–P4同步验收、P5完善中、P6未完成。整体百分比尚无冻结分母。

正常还原已接入耐久Intent/逐写journal、实际释放后的receipt与启动只读隔离；同进程失败阻断新权并保留原cleanup。daemon 2806项通过、2个进程helper另验，定向91项、真实死亡/重启8阶段16进程通过。接口294/294由Rust接管。Release后端及包内验证通过，复用未变581 GUI/Swift；精确清理2条缓存路径、1696608256字节。未安装、无DMG、无实机。独立启动恢复权利/持久物理绑定、legacy共享生命周期及完整实机/长稳/升级/性能仍待验收。

正常显式restore在任何自有音频清理前持久记录绑定真实root、原备份哈希、模块身份、完整current/target配置及有限预约的Intent。实际registry没有清理义务时不新发现或停止未知ADB helper；已有exact/unknown义务仍沿原计划真正结算，未确认不继续。

typed restore预约不能降级为普通无journal权利。每个USB/serial持久写入前真实同步一次Prepare；实际payload/ACK、known rejection、NotSubmitted、unknown及typed readback分别记录。after-drain guard重复核验原token但不重复Prepare；落盘/校验失败阻止后续setter并保留原unknown。VoLTE与IMS分步回读，最终严格目标确认及原预约实际release之后才记录receipt/归档。

Gate的nonClone guard仅暂拒新OperationRight，既有Plan/精确cleanup不因自身journal自锁。归档、原root、输入和namespace确认后才complete并发布last_restore；失败Drop或未登记begin/attach留下的ACTIVE转单调拒绝。启动held-root scanner对任意ACTIVE形状隔离，诊断只读且有界；归档不能证明其他legacy历史clean，不自动replay或解封。

生产native serial reconnect改为在原scoped会话上先initialize，再typed CGSN/ATI确认。所有Unknown fence保留；same-IMEI串口重开仍不能代替已验证native USB successor来洗掉restore reboot Unknown。该修正不授予新的实体设备身份，也未实现持久物理binding。死亡门夹具另保存真实user_configured偏好，生产RF gate未放宽；真实只读AT+QADBKEY?给明确不支持ERROR，未允许授权写入。

最终冻结2923项输入，较584变化26项/新增8项。daemon **2806通过/0失败/2忽略**，这2项是仅供独立进程调用的helper，未当作单测通过；定向91项、FS8项、API常规9项及专项DTO1项、独立进程集成10项、包内17场景23进程、handoff3场景8进程、日志退出3模式与正常EVS10模式向量均通过。实际passed用例增删及净增39项见[新增证据](../../../../build/reports/iteration-585-new-test-evidence.json)。没有声称整套workspace/全部Swift程序本轮重跑。

真实生产Service→Plan→NativeTransport字节/私有PTY及journal链分别运行到Intent、Prepare-before-I/O、actual-I/O-returned-before-terminal、第二voice、target-verified-before-release、released-before-receipt、receipt-before-archive和archive-before-publish。父进程只SIGKILL自有PID并reap，独立新进程读取同一run/sequence，普通业务拒绝、零factory/重放、证据字节不变。最终矩阵8次故意SIGKILL是验收动作，与超时/强制收尾失败区分；最终16个owned进程全部确认，未发生强制收尾。见[最终死亡门](../../../../build/reports/iteration-585-final-v9-restore-journal-crash.json)。它不证明断电或native硬件恢复。

原失败全部保留：v1/v5编译失败；v2/v3/v4/v6定向90通过/1失败；v2筛选位置错误在运行前拒绝，v3 Swift复用harness引用错旧manifest。首次死亡门前4阶段通过，第5阶段因夹具默认RF抑制未到边界；[原报告保持false](../../../../build/reports/iteration-585-restore-journal-crash.json)。[v8失败](../../../../build/reports/iteration-585-final-v8-restore-journal-crash.json)同样前4阶段通过，第5阶段未建模真实只读AT+QADBKEY?，panic/poison导致30s边界超时及自有进程强制清理，原forced_cleanup/timeout保留true；不能称所有历史进程均无强制退出。最终成功不覆盖这些记录。精确报告及loghash见[最终门禁](../../../../build/reports/iteration-585-gate-summary.json)与[软件结果](../../../../build/reports/iteration-585-final-software-result.json)。

候选只重编Release Rust后端，未变581 GUI/Swift按源/实际二进制指纹复用；arm64、严格ad-hoc签名、许可、仅Rust/libusb及生产包无test hook检查通过。后端SHA256：`2716ab143fc469d30af7496f74f9050d4cdf6e9911867d54aa7605e2ffd8be6e`。所有验证结束后精确清理2条编译缓存、1696608256字节；源码/依赖/基线/runtime/产物/正式dist保留。

独立RecoveryOwner/Coordinator、有限RecoveryRight、可重启后重新证明的持久物理binding与显式恢复策略尚未实现；普通journal不是启动恢复权利。legacy/shared guardian、旧3.42 exit1、未受控旧历史、真实USB/串口/no-AT/AT-ADB关系、当前候选注册/SMS/USSD/provider/来去电、9eSIM/LPA、多设备Profile、自然期限/24h及最终采用的72h门槛、原位升级/Keychain/性能和最终交付仍待验收。serial+拓扑是操作性连续性证据，不是加密设备身份。详见[剩余验收审计](../../../../build/reports/iteration-585-remaining-acceptance-audit.md)。本轮未触卡、发信、拨号、改变Profile、安装候选或生成DMG。
