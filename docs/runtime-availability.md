# 持续服务的网关故障隔离

2026-10-16 原生App逐目标可用性按选择性Git构成完成完整79项／13套件，零失败／零跳过，原2项真实Rust集成在同一轮启用并通过，iOS无签名构建通过。6产品／2测试／2真实DTO输入精确合入，其他未提交App改动保留；此前混合工作树85加2与本次79明确分开，没有删除测试改变范围。失效目标禁用、健康目标继续可用、保留历史反馈且不补发旧操作；新手机包和六键视觉／硬件验收仍待。见[原生App资格](evidence/stage-e-browser/native-partial-availability-root-check.json)。

2026-10-16 后续runtime10已按852冻结输入完成1197项完整Rust、172实际程序及严格Clippy核验，原1160全部保留；相同正常CLI的原HTTP8/8、加密HAP8/8、WSS、延迟、旧回执取消和新父Keypad可用性13条记录全部exit0。48个Rust／测试文件已精确合入工作树，README和原脏手动工具保留；Linux本机交叉编译与真实schema27整状态升级／回退已交部署，现网尚未切换。原生App私有85项加原2项真实Rust集成及真实URLSession部分状态读取链通过，最终选择性Git构成另行核验，尚未发新包或验收手机卡片。原65／208／24源语义和用户挂起现场项目均保留，D/E未完成。见[后端资格](evidence/stage-e-browser/multi-gateway-runtime10-root-check.json)、[原生部分可用性](evidence/stage-e-browser/native-partial-availability-root-check.json)。

2026-10-16 后续私有runtime7的852冻结输入已完成完整1189项回归，原1160按package／target／函数全部保留、172实际程序重哈希；严格Clippy仍两错101。原HTTP8/8及WSS部分推送通过，HAP八格仅4绿（健康B实测45仍可能显示旧30），普通延迟已保存原作用域但健康B到期及恢复后新B仍失败，两完整延迟／取消契约exit2。未改原契约、断言或期限，也未据单格通过宣布完成。实际设备availability／action_gate已交App，手机视觉与原生适配仍待；候选未合入／部署，现网1de及65／208／24剩余源语义范围保持。见[后续候选独立核查](evidence/stage-e-browser/multi-gateway-runtime7-root-check.json)。

2026-10-16 首轮私有运行时runtime1已按852冻结输入独立复验：原HTTP八组合（无回复/部分回复/EOF/RST，各两排序）全部通过，B同连接、真实反馈及新控制保持，A拒绝且恢复不重放。加密HAP扩大到八组合后仅3绿；五例B模块45已确认但HAP仍30，虽八例均保持同一认证会话、身份、B50和恢复后新A60写出，也不能宣布HAP整组通过。WSS仍全局unavailable并在同transport2下reset；普通未分组延迟场景的作用域未落库，到期Failed，恢复后新B也无法执行，完整延迟/取消契约exit2。所有原契约、期限和失败保留，没有增加等待或改断言。尚无此候选完整Rust/Linux资格，未合入或部署；当前1de及原1160资格保持，65/208/24源语义仍待审。见[首轮私有候选核查](evidence/stage-e-browser/multi-gateway-runtime1-root-check.json)。

2026-10-16 取消身份的最小私有修复已限定核查：每个新Control请求通道分配独立UUID，clone/不可用视图保留；新场景回执记录runtime_instance，取消先核对boot/rev/实例，缺失或不同实例409，原请求格式不变。原3项Web场景及新增1项回归通过，包含四种无效实例、同实例合法取消/重放，严格全目标包Clippy通过。实际普通CLI在同一固定反例中仍重用数字run1，但旧A取消已409、新B精确UUID保留并到期Written，随后新B延迟动作也写出。完整隔离契约仍101，不能将此小修复当作完整多网关通过。补丁交由整合修复，尚未合入/部署/全工作区回归。见[私有取消修复](evidence/stage-e-browser/stale-scene-cancel-private-fix-root-check.json)。

2026-10-16 原取消回执与新场景发生实际别名：正式CLI同boot/rev恢复后，旧A和新B数字run均为1，但持久UUID不同；引用旧A start_request_id的取消请求返回cancelled1，并把新B精确UUID从pending改为cancelled/cancel_requested，到期没有新B60命令或线帧。另一个新B延迟场景随后真实Written，排除定时路径不可用。现有API只校验boot/rev、运行时只校验数字run/controller，不能保证原目标身份；须绑定持久身份或不可复用代次，保留合法取消及来源授权。实际契约101、后端SIGTERM0，无夹具错误；仅证同测试管理员误取消，不声称跨角色扩权。修复私有进行，现网未改，D/E继续开放。见[旧取消回执反例](evidence/stage-e-browser/stale-scene-cancel-root-gap.json)。

2026-10-16 延迟场景隔离已补实际反例：两个独立回环AD8各接受一个5秒后60%的场景，源UUID逐项对应API last_run；仅A故障即让纯B场景也以transport_failure取消，到期零B60帧。恢复后新B40即时命令和新B60延迟场景均有真实线帧/对应Written账目，旧A动作没有恢复重放。严格契约101、后端正常SIGTERM0、无假网关或未分类探针错误；早期AD8指令模式和数字句柄/UUID夹具错误单独保留。修复须保留健康网关既有时间线及其原时限/授权，不能只让新即时开关可用；共享预算、STOP和发布屏障仍须保留。未改现网，D/E仍开放。见[延迟场景反例](evidence/stage-e-browser/multi-gateway-delayed-scene-root-gap.json)。

2026-10-16 加密HAP局部故障反例已独立复现：正式CLI先完成真实Pair-Verify、加密读取/订阅及B30控制；仅A假网关RST后，健康B也返回-70402并收到不可用EVENT，B50没有写出。A完整恢复后REST新B40可执行，配对身份不变，但同进程重建HAP监听并关闭原认证连接。严格契约与HAP客户端退出101，后端按SIGTERM退出0，无假网关或未分类夹具错误；不能把保留身份当作保留会话。新A60恢复控制门槛因会话关闭未执行，未记通过。运行时隔离继续私有修复，未改现场；D/E仍开放。见[加密HAP反例](evidence/stage-e-browser/multi-gateway-partial-hap-root-gap.json)。

当前正式程序的部分WSS反例也已实际复现：两条本机假总线初始化成功，A连接重置后，B仍发送真实反馈，原认证WebSocket不关闭，却收到全局unavailable；B45/B40未继续显示，A恢复后全局transport从2变7并reset。正确隔离须保留B的同连接、真实状态和新控制，也不能用局部健康替代授权或把未知电机位置当断线。私有基础层已通过限定Core/Storage与严格迁移检查，运行时尚未贯通；不是新现场故障，也未改变现网。见[WSS反例](evidence/stage-e-browser/multi-gateway-partial-wss-root-gap.json)。

2026-10-16 接收RST补充：新正式程序已关闭审计成功后仍永久停止准入的退出路径；普通读及待完成模块读回均可交给既有新连接同步。只有ConnectionReset／ConnectionAborted／BrokenPipe／NotConnected且内存仲裁与持久日志均成功才允许恢复，真实取消、存储/仲裁失败、非网络错误和投影不变量不能被网络重试掩盖。最终1160项及严格Clippy、两项实际原生API联调通过；字节相同正式程序的88路/常驻/RST生命周期证据保留，Linux隔离启动、旧库兼容、实际回环RST生命周期及常驻更新已核验。此修复不改变下述整组所有者结构，独立多网关控制契约仍失败。见[限定修复证据](evidence/stage-e-browser/receive-network-error-root-check.json)。

Root两次独立读取确认新版同PID、零重启、无限期限并保持原身份、配置及永久Admin。较晚新增一条HAP配对已与原管理员授权日志核对，原记录无改删；初始全表比对失败和后续逐条证明均保留。见[Linux交付及身份边界](evidence/stage-e-browser/receive-network-error-linux-deployment-root-check.json)。

2026-10-16 当前状态：整合服务已常驻上线，单网关恢复资格见[最终部署](evidence/stage-e-browser/indicator-final-linux-deployment-root-check.json)。Root新增两个独立本机TCP网关反例：仅A查询失去反馈，健康B仍发送13次反馈，但新B控制只得到unknown、没有新增journal或线帧；A恢复后新B控制再次成功。管理监听与服务本身保留，程序最终按SIGTERM正常退出。当前实现会因一条总线故障drop整个owner、重新连接两条总线并关闭全局准入，因此下述“其他独立网关不随之关闭”要求仍未实现，私有修复在做；不以单GW存活替代该门槛。详见[多网关反例](evidence/stage-e-browser/multi-gateway-isolation-root-gap.json)。

2026-10-15 用户明确要求：网关反馈查询超时不应使整个 HomeSense 持续服务退出。旧版已独立复现，修复已通过完整本机回归与正式程序恢复检查，Linux验证及部署仍在进行。不能以进程托管自动重启或调大重试次数代替故障处理。

在已建立服务的运行期，网关断连、周期查询超时或恢复同步失败时，原生 HTTPS／WSS、会话与诊断继续提供可用性信息；相关设备和场景显示不可用或未知，旧反馈不作为实测，控制保持关闭。HAP 保留既有配对身份并报告不可用，其他独立网关与接入通道不随之关闭。

恢复采用有界、退避的尝试，不阻塞接口、不忙循环、不无限积压日志。重连后必须完成新代次的可靠反馈同步才能重新接受控制；恢复不重放启动规则、旧灯光／窗帘命令、原生翻转或已取消任务。显式停机和整体测试截止仍应及时结束；存储损坏等真正致命错误另行报告，不能被网络重试吞掉。

验证须运行生产 CLI 与本机模拟网关：长时间无反馈／断连期间接口仍在线且控制拒绝，重连后反馈不全继续禁控，完整反馈恢复后只允许新请求；同时验证重试节拍、停止／整体截止、资源有界和配对身份。查询失败诊断应带模块／通道范围、缺失端口和连接代次，不能仅报网关超时。

真实现场旧版在约17:28查询超时后提前退出，Root已确认进程与端口终止。用户估计在相近时间用墙面板开灯，当前日志不足以证明因果，不应把正常面板操作当作错误使用。没有要求用户重放动作。见[退出证据](evidence/stage-e-browser/runtime-gateway-timeout-root-gap.json)。

独立软件反例已复现：正式 CLI 在纯本机模拟 AD8 的 8 路反馈全部新鲜后，仅屏蔽查询回复；24 秒运行窗口尚未到期，约 5.1 秒后进程退出，HTTPS 拒绝连接。该例只有 1 路绑定逻辑灯，其余 7 路仍纳入同步，且未发送控制命令。这证明反馈超时可终止服务，不能证明真实现场超时的根因。见[正式程序反例](evidence/stage-e-browser/runtime-feedback-outage-cli-root-check.json)。

已配对加密 HAP 回归已纳入最终1117项完整回归并通过：无反馈与仅绑定通道有反馈时持续报告不可用，完整八路新代次反馈后，使用原配对再次认证，AID／IID 与长期身份不变；仅新的控制执行，反馈及订阅事件均恢复。相同测试在旧版的无反馈阶段因服务终止而遇到加密连接 EOF。另有 AD8 自动化原生 Ramp 与离线校验专项通过。详见[恢复核查](evidence/stage-e-browser/runtime-availability-recovery-root-check.json)。这是本机合成协议证据，未部署、未操作现场；多个 HomeSense TCP 网关之间的独立控制隔离仍未完成，不能用单网关存活证明。

最终冻结输入的完整workspace 1117项／169程序、文档10个无示例结果块及严格全部目标Clippy通过；正式AD8程序在40秒纯回环生命周期中保留同一WSS与会话，旧请求不重放，完整新代次反馈后新动作才执行。发布后部分反馈的加密HAP不可用路径和恢复查询轮换均通过。保留各旧版本失败日志，不把前期定向成功代替最终整合。

用户随后要求E2一直提供服务。显式 `forever` 模式已实现，以无整体截止表示常驻，保留1–86400秒有限模式及操作级上限；真实信号停止和30天逻辑时间推进专项通过，本机1128项完整回归、文档目标和严格Clippy通过；已完成Linux生产交叉编译与隔离烟测，systemd常驻启动并经Root独立可信HTTPS就绪核查。首次完整反馈之前保持starting、尚不提供监听；完成初始化后的故障保留监听和诊断。systemd采用当前已提交配置的一致快照启动，避免App发布新版本后被旧文件阻断；不会以每日重启续期。见[服务生命周期](service-lifecycle.md)。
