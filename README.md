# HomeSense · 知居

<img src="docs/assets/homesense-logo-v3.png" alt="HomeSense 知居 Logo：橙色家庭大脑连接琥珀色照明、蓝色自动化与绿色节能节点" width="160">

HomeSense（中文名：知居）是原生 Rust 家庭中控，通过 TCP485 网关控制 RS485 模块，并通过原生 Rust HAP 接入 Apple 家庭。目标是逐步替代现有的Control4 EA-5 的灯光、窗帘、场景与事件逻辑。

## 当前实现

截至 2026-10-11，阶段 A–C 已完成设计规定的实现及限定范围验收，阶段 D 仍在进行。完整证据与未关闭问题见 [A–C 验收跟踪](docs/stages-a-c.md) 和 [阶段 D 完成审计](docs/stage-d-progress.md)。

当前目标推进到阶段 E：原生 HAP 设备与场景入口、Web 管理和鉴权，同时补齐阶段 D 的运行时整合。实现按可验证的模块分步提交、推送，并同步本 README；完成条件见 [阶段 E 验收跟踪](docs/stage-e-progress.md)。离线测试通过不等同于现场验收或接管。

- **协议、状态与控制**：二进制编解码、分片接收、型号与验证门槛、desired/observed 分离、有界队列、STOP 预留、持久化准入和唯一传输所有者；包含断线恢复、周期查询、配置发布及存储故障屏障。
- **Apple 家庭**：原生 HAP 桥与灯光配件，持久化身份、配对和 AID/IID；支持加密读写、订阅通知及反馈失效投影。已有 iPhone 配对、重启身份和书房主灯组合控制的限定现场证据。当前 HAP 图只提供灯光，窗帘通过独立逻辑控制入口执行。
- **Control4 导入**：读取 C4P／project.xml，保留源指纹、对象和程序树，生成运行配置、覆盖报告及人工确认清单。原项目 136 条设备映射、68 场景、215 程序均纳入处置；导出设备和候选规则默认禁用。
- **自动化配置与语义导入**：严格校验类型化变量、反馈缓存、规则、手势、日历和定时场景；启用项需要审核证据。Control4 程序转换保留条件与源位置，不支持的语义明确阻断，候选规则默认禁用。配置／导入 35 项测试和严格 Clippy 通过；统一模拟服务已接通；真实服务整合仍待完成。
- **场景与规则**：条件 AST、变量事务、延迟场景、取消、手势和上海时间日历已接入统一因果运行时。启动、定时、面板、设备变化和手动场景结果通过批量根共享预算；规则启动的延迟场景沿原链续接，变量派生事件同链执行。书房三个场景有手动执行、重复关闭和取消的限定现场证据；新增自动规则尚未部署，EA-5 控制范围不变。
- **自动化运行时**：已发布文档通过完整校验后建立触发索引，执行时读取原动作。变量、立即命令与延迟场景收据原子接受，调度容量提前预留，成功／失败／取消可追溯；模拟服务验证延迟执行、原链续接和重启不重放。设备状态仍以新鲜反馈为准。真实 TCP 服务发布监督、输出反馈因果关联、全项目语义审核和阶段 E 界面尚未完成。
- **配置成组发布**：存储 API 将设备配置、自动化文档及持久变量声明在同一事务中发布；同名同类型变量保留，类型冲突拒绝。旧设备发布入口不能静默丢弃自动化，启动重新校验两个文档。已提供停机发布 CLI；AutomationRuntime 可从准入管理器读取已发布版本并装配面板、日历和场景，提供场景启动、取消、状态、释放运行句柄及持久取消退出；进程内有界控制通道可在循环运行时启动／取消／查询／释放场景，队列满立即拒绝、关闭释放等待请求；LastRun 查询从持久记录读取当前配置场景的最后一次执行，运行句柄释放后仍可查，保留 boot、版本和可选规则因果信息以区分时钟与来源，按执行 ID 查询也限制配置版本；Activity 查询独立核对保留运行的实际目标，缺失反馈为 Unknown、句柄释放后返回空；SceneActivity 可按启用的已审核定时场景名称查询目标状态，不依赖运行句柄、不触发执行；准入层已支持成组发布屏障，旧自动化可在确认发布屏障后暂停退出，保留提交前的旧映射 STOP 排空；新增发布恢复装配：校验新连接代次，重建反馈并完成全映射查询后才恢复控制；查询失败保持关闭；未授权、旧代次及网关缺失的恢复请求均拒绝，并保留后续合法恢复机会。模拟服务已接通一次发布的暂停、排空、提交和新映射查询恢复，并验证旧延迟动作不重放；真实服务监督仍待接通。
- **自动化持久记录**：schema 13 保存类型化变量、日历水位、规则因果审计、延迟场景调用来源及场景运行／步骤／命令关联；提供一致分页查询，重启将未结束场景标为中断，不重放。新 schema 尚未部署，旧版本程序不能直接打开升级后的数据库。存储回归 25 项通过。

完整工作区回归：368 项通过，0 失败、0 忽略（schema 13，测试版本 52df2fe，包含既有未提交代码改动）；指纹与范围见 [集成证据](docs/evidence/stage-d-study/causal-service-regression.json)。全 workspace 严格 Clippy 尚未通过，vendor/hap 既有警告仍待处理。

EA-5 配置与控制范围保持不变。nanopc 已有候选构建、隔离测试和限时现场试验；正式 systemd 部署、持久发现、72 小时运行及全屋接管仍属后续阶段。历史查询超时和窗帘偶发无动作的原因尚未关闭，成功复测不代表长期稳定性验收。

## 构建与离线验证

使用 Rust edition 2024，最低 Rust 1.99。依赖由 `Cargo.lock` 锁定，SQLite 使用 bundled 构建，HAP 使用仓库内受控的 `vendor/hap`。新环境先获取依赖；缓存齐备后可离线构建。

```sh
cargo fetch --locked
cargo build --locked --offline --workspace
cargo test --locked --offline --workspace
cargo clippy --locked --offline --workspace --all-targets -- -D warnings

cargo run --locked --offline -p homesensed -- check-config config/simulation.json --simulation
cargo run --locked --offline -p homesensed -- check-automation config/simulation.json config/automation.simulation.json --simulation
cargo run --locked --offline -p homesensed -- serve-automation-simulation config/simulation.json config/automation.simulation.json /tmp/homesense-automation-demo 10
cargo run --locked --offline -p homesensed -- demo 30
cargo run --locked --offline -p homesensed -- decode 'AE 15 D1 1E EE'
cargo run --locked --offline -p homesense-import --bin study-replay -- config/scenes/study.json 5 0
```

`serve-automation-simulation DEVICE_FILE AUTOMATION_FILE STATE_DIR SECONDS [SCENE]` 使用内存网关和独立的 `automation-simulation.sqlite`，不打开现场运行库或配对目录；可指定已启用的定时场景。重启要求文档完全一致，改变配置请使用新模拟目录。

上述工具不连接真实网关；`check-automation` 只校验结构、引用及启用规则的命令编码，`study-replay` 只输出计划。工作区集成测试需要允许本机回环监听。

## 模拟与现场入口

本机模拟 HAP 服务使用内存网关，只监听 `127.0.0.1`，不发布 mDNS。以下运行 60 秒，状态与身份保存在独立目录：

```sh
cargo run --locked --offline -p homesensed -- serve-simulation config/simulation-home.json /tmp/homesense-simulation 51826 60
```

`serve-simulation-lan FILE STATE_DIR PRIVATE_IP:PORT SECONDS` 可在指定私网地址发布 mDNS，供 Apple 家庭模拟联调；仍不连接配置中的真实网关。服务时长上限为 24 小时，重启保留身份和配对，已有状态目录要求配置与监听端口匹配。

现场工具包含 `observe` 被动采集、`query-status` 单通道查询、`query-parameter` 参数查询、`motor-test` 开／关及 STOP 校准、`panel-led` 手动 LED，以及 `serve-live` 限时灯光桥。真实入口使用现场配置及显式确认参数，具体操作与证据见 [nanopc 部署记录](deployment/nanopc/README.md)；书房场景入口见 [阶段 D 书房迁移](docs/stage-d-study.md)。配置变更通过 `publish-config FILE STATE_DIR EXPECTED_REVISION` 在停止服务后发布；已有自动化的版本使用 `publish-automation DEVICE_FILE AUTOMATION_FILE STATE_DIR EXPECTED_REVISION` 同时发布两个文档。先备份运行库；命令要求服务停止，完成后也保持停止，不自动启用规则。

`config/import/` 为禁用的导入草案，`config/live/` 为限定现场配置，`config/scenes/` 为书房迁移及调试资料。当前代码支持的运行库为 schema 13，尚未部署到 nanopc 原配对运行库；升级与回退须匹配数据库版本，保持身份库中的最新配对和撤销状态。

## 项目结构

| 路径 | 职责 |
| --- | --- |
| `crates/homesense-core` | 设备类型、命令仲裁、场景、规则、手势与日历核心 |
| `crates/hb-protocol`、`crates/hb-transport` | HopeBeauty 协议与有界 TCP 传输 |
| `crates/homesense-state`、`crates/homesense-simulator` | 状态及反馈投影、确定性网关模拟 |
| `crates/homesense-storage`、`crates/homesense-config` | SQLite 日志与变量、设备及规则配置校验 |
| `crates/homesense-hap`、`vendor/hap` | HAP 适配、身份存储及受控 HAP 实现 |
| `crates/homesense-import` | Control4 导入、处置报告及离线场景回放 |
| `crates/homesensed` | 统一准入、传输监督、服务和调试入口 |

## 资料

- [完整设计](docs/design.md)：v1.7，系统边界、协议契约、迁移方案及阶段 A–G 计划。
- [A–C 验收跟踪](docs/stages-a-c.md) 与 [逐项证据](docs/evidence/stages-a-c-requirements-20261011.md)：限定验收结论及未关闭问题。
- [阶段 D 完成审计](docs/stage-d-progress.md)：当前实现、验收缺口和增量验证记录。
- [书房场景迁移](docs/stage-d-study.md)：源规则、离线回放及手动场景现场证据。
- [实施记录](docs/implementation.md)：实现过程、测试与历史待办；当前阶段结论以上述审计为准。
- [nanopc 部署记录](deployment/nanopc/README.md)：候选版本、隔离测试、身份与回退边界；实时运行状态须另查进程。
- [协议核对记录](reference/control4/hb-protocol-notes.txt)：厂家报文依据和未验证事项。
- `reference/control4/`：Control4 解析器、解析 JSON、报告、HopeBeauty 地址映射、手册文本与原页截图。
- `reference/manifest.json`：每份资料的来源、复制时 SHA-256 和当前 SHA-256。设计文档及协议笔记更新后的哈希单独记录，原始复制哈希继续保留。

原始项目仍在 `/Volumes/m4work/aiml/Control4/Projects/Charlies250801.c4p`；参考目录是离线快照，不是运行配置。完整解析 JSON/报告包含私人住宅资料，默认列入 `.gitignore`，发布前需另行审阅。旧资料保持原位，复制时逐文件核对哈希。

## 协议边界

普通帧为固定 5 字节二进制 `AE ID CMD VALUE EE`，无文档定义的校验和。地址按两位十六进制解析；禁止广播地址 00；亮度是整数 0..100。按注册模块类型区分开关反馈与面板按键，不为未知帧产生设备事件。面板 LED 依赖已处于手动模式，库不修改模块模式。

帧接收器支持拆包、粘包和逐字节恢复，内部至多保留四字节；payload 内的 AE/EE 不作为分隔符。无校验和意味着损坏数据偶然匹配起止字节时不能保证识别，后续语义分类是必要约束。断链应调用 `reset()` 丢弃残帧。

窗帘只提供开／关／停，不推算位置百分比；逻辑方向通过逐通道校准映射到电机升／降／停。书房两个方向及 STOP 已有限定验收证据，其他通道仍须独立校准。命令接纳或写出不等于设备已完成动作；灯光状态以新鲜实际反馈为准，未知、过期或断线时报告不可用。

项目中的品牌全称统一为 `HopeBeauty`，代码与模块缩写使用 `hb`。原始 Control4 驱动标识、厂家资料路径、手册摘录和归档设计保留来源原文，解析器仍按原始驱动标识识别设备。

会话失败终态：显式 Session 在启动或结果续接失败时取消原因果链、尝试持久保存失败原因，并关闭准入；已接受变量和动作不回滚。失败审计自身写入失败时返回双重错误，不伪报已记录。64 项运行时回归及 homesensed 库严格 Clippy 通过，故障注入覆盖循环拒绝后变量保留、失败记录及审计不可用。future 被取消或 Drop 仍只能同步关闭准入，未完成收据留给恢复流程；自动事件循环与多根路由仍待接入。EA-5 配置及控制范围不变，本次未部署。

会话主动取消：Session::cancel 核对调度器和每个运行的持久身份，取消本链未来／排队步骤并保存取消终态；成功后清除待完成映射，释放会话不关闭全局准入，已接受变量和已在写出的命令不回滚。失败或取消 future 仍关闭准入。64 项运行时回归与 homesensed 库严格 Clippy 通过，覆盖延迟动作零输出、变量保留、取消记录、重复取消拒绝及准入保持可用。调度器仍保留取消结果供消费；自动路由器须识别已取消链，不能将其作为新根重启后继规则。自动循环接线尚未完成，未部署、未修改 EA-5。

延迟会话结果路由：新增显式有界 Router，持有最多 64 个根会话，将调度器运行 ID 与持久 audit ID 对应的终态送回原 Session。取消后保留有界结果标记，终态到达只消费、不触发后继；标记占用容量直到结果消费。未知结果返回未认领，不自动创建新根。64 项运行时回归及 homesensed 库严格 Clippy 通过，覆盖正常续接保留相关 ID、取消结果抑制、满容量拒绝、消费后释放及重复结果未认领。此组件尚未接入自动事件循环；不能据此宣称自动延迟规则已上线。README 与阶段记录同步，现场及 EA-5 不变。

延迟路由轮询：Router::poll 使用 Admission 单调时钟推进场景，消费终态并沿所属会话续接；在后继规则申请容量前释放已完成场景句柄。未认领结果返回调用方，不推断为新根；轮询失败或 future 被放弃时关闭准入。64 项运行时回归及 homesensed 库严格 Clippy 通过，验证真实单调时钟到期、写出完成后续接、会话清理和调度器容量释放。仍需将此轮询入口与面板／日历事件统一接入服务循环，尚未部署，EA-5 不变。

因果路由关闭：Router::close 先封闭新根入口，再逐个尝试取消活动会话，保存各链取消终态；不推进场景或执行后继规则。正常关闭及重复关闭保持 Admission 可用，供外层完成传输收尾／发布屏障；任一失败或 future 放弃则关闭准入。64 项运行时回归与 homesensed 库严格 Clippy 通过，覆盖关闭后新根拒绝、待执行场景取消且零输出、结果消费和正常释放。服务统一事件接线仍待实现，现场配置不变。

运行循环因果路由装配：AutomationRuntime 持有 Router，提供显式 start_rule／cancel_rule，循环使用 Router::poll，已认领结果不再交给旧 scene_result 根入口；未认领手动结果仍交给日历事件入口。停止时先关闭因果会话再停止调度器，加载本身不执行规则。66 项运行时／模拟服务测试与 homesensed 库严格 Clippy 通过，覆盖显式变量规则、关闭、模拟发布和重启。配置加载仍使用原立即规则校验，自动面板／日历根事件尚未迁移，因此包含延迟调用的自动化文档仍不能通过该服务入口启用；后续需统一事件路由，不能声称延迟自动规则已上线。未部署，EA-5 不变。

同事件批量根规则：Session／Router 新增 start_rules，同一生产事件匹配的至多 256 条不同规则共用根 cause、截止时间和预算；批量执行根规则后统一派生变量事件并保存一个链终态。空批次／重复规则拒绝。65 项运行时测试及 homesensed 库严格 Clippy 通过；新回归验证两条规则同相关 ID、不同事件 ID，节点预算含分发成本且不能按规则重置，预算不足保留第一条已接受变量、拒绝第二条并记录失败。日历／面板生产端尚需切换到批量入口，未部署。

日历生产端因果迁移：提取持久水位提交后的 occurrence 读取，新增 poll_causal，将同一时刻事件匹配的规则交给共享批量根入口；AutomationRuntime 循环采用此路径。水位仍先于规则接受持久化，不重试部分执行；独立旧 poll 保留兼容。67 项运行时／模拟服务测试与 homesensed 库严格 Clippy 通过，包含两条定时规则共享节点预算、部分接受失败、重复同刻轮询无执行，以及原有重启水位测试。自动面板／启动／设备反馈事件和延迟文档加载仍待迁移，尚未部署。

手动场景结果因果迁移：服务对未被会话认领的结果核对当前启动／配置版本下的持久终态及场景名，只有无规则 cause 的手动场景才创建批量根；孤立的规则场景结果拒绝重启为新链。匹配的多条后继规则共用因果预算。67 项运行时／模拟服务测试及 homesensed 库严格 Clippy 通过，模拟 CLI 两次启动验证每次两条后继规则共享一个相关 ID、变量落库且不逐 tick 重复。自动面板／启动／反馈事件和延迟文档加载尚待迁移，未部署、EA-5 不变。

启动事件因果迁移：运行循环进入已同步的 AutomationRuntime 后，一次性通过共享根触发启用的 startup_synchronized 规则；提交前标记已尝试，模糊结果不在后续 tick 重试，停止标记阻止后续启动。配置加载不触发事件。67 项运行时／模拟服务测试及 homesensed 库严格 Clippy 通过，两个独立模拟启动各执行两条启动规则且共享一个相关 ID，不逐 tick 重复，最终变量落库。面板／设备反馈事件与延迟文档加载仍待迁移，未部署，EA-5 不变。

设备反馈生产端迁移：抽取共用 feedback_batches，保留游标、整批时效／代次验证和组合灯投影；服务绑定当前反馈游标，后续每批实际变化通过共享根执行匹配规则。旧独立反馈 API 复用相同提取逻辑。65 项运行时及 2 项模拟服务回归、homesensed 库严格 Clippy 通过；模拟服务两次启动均由新反馈触发变量落库。协议缺少命令标识，输出引起的原始反馈仍不能可靠继承原命令因果链，此边界未解决。面板迁移及延迟文档加载仍待完成，未部署、EA-5 不变。

面板手势因果迁移：服务经 consume_feedback_causal 消费按键边沿及到期手势，同一手势匹配规则使用共享批量根；长按任务启动／松手释放仍先沿原路径处理，Observe 模式及独立旧接口保持原行为。Outcome 增加规则 ID，面板报告保留根及派生规则身份。66 项运行时及 2 项模拟服务回归、homesensed 库严格 Clippy 通过；专项验证真实反馈帧生成一次手势、两条规则共享相关 ID、变量顺序提交及重复消费无输出。延迟文档加载仍需切换校验路径，原始输出反馈因果关联仍未解决，未部署、EA-5 不变。

延迟规则服务加载闭环：AutomationRuntime 使用完整 deferred 链接校验后生成仅含触发索引的适配器，原动作保留在已发布文档，所有服务执行入口经因果路由读取原规则；旧独立立即适配器不变。发布校验识别启用延迟定义并继续校验其每步编码。66 项运行时、3 项模拟服务测试及 homesensed 库严格 Clippy 通过；新增 CLI 两次启动验证“启动规则→100ms 延迟输出→结果规则”的原相关／父事件继承、各启动一个成功终态、变量落库及不触碰现场数据库。反馈缓存别名已由链接器转换为设备反馈读取，并接受新鲜度和命令冲突检查；输出反馈因果关联、完整阶段验收及真实服务部署仍待完成；EA-5 不变。

规则失败分类基础：延迟适配器新增 execute_classified，返回 Planning(core 链／求值错误) 或 Runtime，而原字符串 API 保持兼容。Planning 仅证明当前调用在准入前拒绝，不代表此前同链动作可回滚；容量、准入和安装失败仍保守归为 Runtime。66 项运行时回归及 homesensed 库严格 Clippy 通过，验证预算拒绝与容量故障分类、变量均未提交。当前 Session 仍按原策略关闭准入，链级故障隔离尚待实现，不能将此分类视作隔离验收。未部署，EA-5 不变。

延迟续接失败分类：新增 resume_scene_classified，结果领取的预算／时钟等链错误保留类型，后继执行复用 execute_classified；若已有后继接受后再失败，以 Partial 携带已接受报告及底层错误，避免后续隔离逻辑误判为整批未执行。原字符串入口兼容且停止策略未放宽。66 项运行时回归及 homesensed 库严格 Clippy 通过，专项验证原链节点预算耗尽时返回 Planning(Budget) 且后继未执行。Partial 清理与链级隔离仍需进一步接入验证，未部署、EA-5 不变。

部分接受记账修复：Session 每接受一个根规则立即登记其安装场景，不再等全部根完成后才登记，后续根失败也保留此前任务清单。新增真实准入／调度回归：延迟终态续接时第一条规则变量已提交、第二条预算拒绝，Partial 报告与持久记录一致；另验证首根已安装任务在次根失败后仍被会话跟踪。67 项运行时测试及 homesensed 库严格 Clippy 通过。当前失败仍关闭准入；类型化错误的会话传播、清理与链级隔离仍待接入，未部署、EA-5 不变。

部分续接会话记账：Session 改用类型化续接入口，在 Partial 分支移除已核验消费的父运行，并登记此前成功后继报告中的运行后再保存失败。避免把父终态误留为待完成任务，失败策略仍保持关闭准入。67 项运行时回归及 homesensed 库严格 Clippy 通过，新增分支验证变量后继已接受、下一后继预算拒绝时父运行清理；此前根批次部分接受跟踪回归保留。链级隔离仍未完成，未部署、EA-5 不变。

会话层规划失败隔离：预算、循环及规则求值拒绝经过类型判定后，先取消本链待完成场景，再持久保存失败；两步成功才标记 failure_isolated 并保持 Admission 可用。清理身份不明、清理／审计失败或其他运行时错误仍关闭准入。67 项运行时回归及 homesensed 库严格 Clippy 通过，覆盖循环隔离、审计故障关闭、部分变量提交保留、已安装延迟任务取消且零输出、丢弃已隔离会话不关闭准入。外层 Router／事件循环仍按错误退出，完整服务链级隔离尚未完成；未部署，EA-5 不变。

服务级规划失败隔离：Router 区分已隔离根失败与运行时故障，事件生产端 dispatch_rules 对已持久审计的隔离失败记录诊断并继续；显式 start_rules 仍返回错误。隔离后取消任务保留有界归属标记，续接隔离同样转换为取消结果消费，不重启规则。67 项运行时、4 项模拟服务回归及 homesensed 库严格 Clippy 通过；新增 CLI 验证启动循环失败落库后，独立手动场景仍写出且后继变量成功。未分类运行时故障、清理或审计失败仍停止服务；更广泛故障策略及真实服务验收仍待完成，未部署、EA-5 不变。

隔离终态回收修复：续接在已核验持久终态后发生规划拒绝时，移除父运行再清理剩余任务，避免调度器已释放父句柄导致误判清理失败并关闭准入。67 项运行时回归及 homesensed 库严格 Clippy 通过，覆盖根批次预算失败后的取消结果消费／容量释放、终态领取预算耗尽后的隔离，以及随后独立规则继续执行；取消结果不会外泄为手动事件。未部署、EA-5 不变，完整阶段审计仍未完成。

因果期限调度前检查：Chain 暴露固定截止时间，Router 每轮推进场景前先使已到期 Session 取消待执行任务并持久记录 deadline 失败，保留取消结果归属并在本轮消费释放容量。67 项运行时、4 项核心规则链测试及 homesensed 库严格 Clippy 通过，故意在链期限与场景延迟都已过去后轮询，验证零输出、审计落库及准入继续可用。此为轮询边界检查，已写出的动作不能撤回，精确到每条命令派发的截止绑定仍需核查；未部署、EA-5 不变。

命令派发期限绑定：延迟规则适配器将原 Chain 截止时间送入准入，立即输出与一次性场景凭据携带该期限；SceneRuntime 每步接受继续传递，实际队列 deadline 取原链期限与通用 TTL 较早值。旧手动接口保持原 TTL。37 项准入、68 项运行时测试及 homesensed 库严格 Clippy 通过，专项分别验证立即／延迟命令持久 deadline 等于链期限，排队过期且不再轮询场景也不能派发。已开始的物理写入不可撤回，现场时序仍需验收；未部署、EA-5 不变。


反馈读取冲突隔离：延迟规则准入现在区分提交阶段失败和接受后的回执读取失败。仅提交阶段明确返回 ResourceBusy 时，在取消已接受的后续工作并保存失败审计后允许服务继续；回执读取失败仍停止准入。反馈缓存别名沿用现有链接器转换，不写入逻辑变量表。同毫秒或未完成命令的反馈仍可能被拒绝，不自动重放。CLI 回归覆盖缓存别名读取、接受或拒绝审计、两个服务启动和独立场景完成；37 项准入、68 项运行时、4 项模拟服务测试通过，homesensed 库严格 Clippy 通过（vendor/hap 仍有既存警告）。未部署现场，EA-5 不变；阶段 D/E 尚未整体验收。


当前工作树集成复验（提交 34dcd3c 加既有未提交代码）：`cargo test --workspace --locked --offline` 共 371 项通过、0 失败、0 忽略，76 个测试结果块。初次执行因沙箱禁止 localhost 监听而中断；获准回环权限后完整重跑成功。证据及代码差异指纹见 `docs/evidence/stage-d-study/causal-service-regression.json`。仅模拟与本机回环，不代表现场部署或阶段 D/E 整体验收。


TCP 自动化服务入口：新增 `homesensed::automation_tcp::serve` 库 API，显式共存确认、Live 配置与规则校验后使用独立 `automation-tcp.sqlite`；先建立 TCP 并完成新代际全量查询同步，再装配统一 AutomationRuntime。运行时限为同步完成后的时长；初始化连接／查询使用各自有界超时。退出或故障关闭准入，不复用 HAP 身份库，不重放旧命令。3 项 localhost 测试验证同步先于启动动作、两次启动各一次规则、既有 runtime.sqlite 保持原样、未审核／未确认配置零状态创建、无初始反馈零规则提交；homesensed 库严格 Clippy 通过。当前仅支持可查询的启用设备；CLI、自动重连、在线发布监督、HAP 合并及现场运行仍未接入，不能视作阶段 D 真实服务验收。


TCP 自动化 CLI 已接入：`homesensed serve-automation-tcp DEVICE_FILE AUTOMATION_FILE STATE_DIR SECONDS --coexistence-confirmed`。此命令会执行审核后启用的规则，使用独立 `automation-tcp.sqlite`，不发布 HAP；SECONDS 为同步完成后的运行时长（1–86400 秒），文件读取上限 4 MiB。仅在准备执行这些规则且已确认 TCP 并发共存时使用，当前尚未在现场启动。4 项专项测试通过，其中回环测试经真正的 CLI 子进程两次启动验证同步门禁、启动动作及独立状态；参数拒绝不创建状态，缺失反馈不执行规则。homesensed 二进制严格 Clippy 通过，vendor/hap 既存警告仍在。自动重连、在线发布与 HAP 合并仍待接入。
