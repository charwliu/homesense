# HomeSense · 知居

<img src="docs/assets/homesense-logo-v3.png" alt="HomeSense 知居 Logo：橙色家庭大脑连接琥珀色照明、蓝色自动化与绿色节能节点" width="160">

HomeSense（中文名：知居）是原生 Rust 家庭中控，通过 TCP485 网关控制 RS485 模块，并通过原生 Rust HAP 接入 Apple 家庭。目标是逐步替代现有的Control4 EA-5 的灯光、窗帘、场景与事件逻辑。

## 主要功能与实现情况

| 功能 | 能力 | 实现情况 |
| --- | --- | --- |
| 灯光控制 | 开关、调光、独立供电组合灯；亮度调节、关灯后恢复亮度；按实际状态禁用重复开／关动作 | 控制、调光与恢复已有限定实机复验；对称按钮禁用软件与限定实机界面复验通过；本地渐变目标有界保护 |
| 场景与自动化 | 场景开关、互斥选择、取消、变量、条件、手势／长按、日历、延迟与渐变；即时场景执行、跨类型联合请求、审核后的停用目标和反馈对账 | 运行时及即时／时间线／选择组联合入口已接入并软件复验；停用链接支持完整目标向量与显式静止策略，后台查询不阻挡；限定场景与实际 LED 联动复验通过，原项目其余语义、外部操作及完整范围仍待确认 |
| 场景实际状态 | 显式审核 All／Any、开关／带电亮度范围、忽略成员与未知策略；只读活动和独立执行入口 | 通用跟踪及 HAP／Web 执行入口已软件复验，接受回执与实际状态分开；原 tracking 编码及实际客户端未知状态显示仍待验 |
| 离家关灯 | 显式审核的按模块全关操作；原生立即全关、完整通道权限、受影响照明任务接管与真实反馈联动 | Web／可选 HAP 入口与运行时已实现；专项回归通过，完整集成稳定性及现场验收待完成 |
| RS485 模块管理 | 独立模块状态、整模块／逐路查询、稳定 `module_id + channel` 引用、组合设备和模块换址 | 软件已实现；96 路周期刷新仍有稳定性缺口，现场查询与换址待验收 |
| Apple 家庭 | 原生 Rust HAP，灯光、选定场景、窗帘开／关／停、可选面板事件、保留配对与多桥 | 限定灯光、场景、窗帘双向短程动作／停止及 iPhone 实际断网后状态恢复已复验，其余客户端范围待验 |
| 窗帘状态 | 同逻辑窗帘共享 HAP／Web 手动控制轨迹；按成功写出时刻计时、可选估计百分比、独立停止键提示与审核 LED 绑定 | 软件接入及独立回归完成；书房 HAP 双向动作、中停、估计终态和 LED 已限定实物复验；iPad Web 自动更新、重复命令和中停百分比已限定复验，其余范围待验，原自动规则保持禁用 |
| 原生 HomeSense App | iPhone／iPad／Mac 的家庭日常卡片、详情控制与配置管理；服务端推送、楼层／房间、草稿及发布 | 原生客户端已入库；家庭卡片、保留页面的模式切换及接口联调通过软件检查，既有调光和目录管理有实机证据；新版完整现场验收待完成 |
| Keypad 设备 | 每块 TS6-86 一张正方形面板卡，两列三行 1–2／3–4／5–6；按键颜色与灯点显示 LED，背光、声音和工作模式为子功能 | 后端及原生 App 联调通过本机检查；旧端点兼容，实际父对象迁移与硬件验收待完成，现网尚未变更 |
| 管理客户端 | 原生 HomeSense App 统一提供查看、控制及配置管理，保留 HTTPS API／鉴权／WSS 推送 | 浏览器页面与静态资源已在后端候选撤除，尚未部署；原生 App 完整管理及控制入口鉴权验收继续 |
| App 连接凭证 | 系统 Keychain 按服务器地址保存令牌，重开免输入，断网保留，支持主动移除 | 保存与恢复已实现；iPhone 完全退出后自动连接及前后台状态同步已实机验证，其他平台现场覆盖待完成 |
| 管理 API 请求幂等 | 控制与管理变更的持久请求记录、端点隔离、重复请求查询和有界过期策略 | 软件验证通过；旧记录保留以拒绝重放，凭证仅首次披露；配置与升级说明见 [请求账本](docs/web-receipt-policy.md)，新版现场验收待完成 |
| 状态与恢复 | 物理反馈与控制目标分离；启动／重连查询、按实际反馈新鲜度调度的周期刷新、配置代次隔离、断线不重放旧动作 | 软件已实现；当前实际总线的恢复与持续运行验证仍在推进；周期查询等待反馈时继续处理请求和状态推送，保留单个待完成读、线速间隔和有界重试 |
| 多房间接入 | 同一 HAP 桥接入各房间灯光、选定场景和窗帘，保留已有配对与设备标识 | 书房和画室已有限定家庭 App 实机验收；画室 Web 验收暂挂起，原墙面板自动逻辑继续保留 |
| 协议与迁移 | HopeBeauty 协议与型号校验、Control4 导入、显式审核场景转换、Agent 场景链接、命令及规则来源审计 | 主要框架已实现；场景转换保留完整来源并输出禁用定义，未知策略挂起；原 tracking、时序与完整项目迁移仍待审核 |

离家模块范围和配置方式见[离家按模块关灯](docs/away-module-off.md)。该操作独立于导入场景，不会自动启用或折叠原场景。

功能实现、软件验证与现场验收的范围见 [D/E 完成审计](docs/de-completion-audit.md) 和 [阶段 E 剩余清单](docs/stage-e-remaining.md)。逐次提交的实现细节与验证记录见 [CHANGELOG](CHANGELOG.md)。

## HAP 与 Web 组合模拟

```text
homesensed serve-combined-simulation DEVICE_FILE AUTOMATION_FILE STATE_DIR SECONDS HAP_LOOPBACK_IP:PORT WEB_LOOPBACK_IP:PORT TOKEN_FILE SCENE...
```

服务先确认 HAP 端口绑定和初始发现记录就绪，再启动自动化；端口占用不会执行启动规则。两个地址必须是不同的本机回环端点；令牌文件权限须为 0600，Web 角色为 member。该命令使用模拟硬件，在同一运行时发布灯光与指定场景，同时提供 Web 灯光和场景控制。它不支持配置激活；现有 HTTPS/WSS 与 TCP 管理入口不受影响。可执行 `python3 tools/verify_web_simulation.py /path/to/homesensed --combined` 验证会话、重启幂等及错误参数拒绝。

## TCP 服务的原生管理 API

原生 HomeSense App 是管理客户端。当前后端候选已撤除浏览器首页与 HTML/JS/CSS 资源，原页面地址返回 `410 native_app_required`；HTTPS API、Bearer 鉴权、配置草稿／预览／发布和 WSS 状态恢复继续保留。现有 `--web-*` 参数名称保留兼容，只表示管理 API 监听。旧浏览器验收记录是历史结果，不表示新候选仍提供网页。

`serve-automation-tcp` 可附加以下参数，启用与总线运行时共用准入、状态和发布监督的 HTTPS/WSS 管理入口：

```text
--web-listen LAN_IP:PORT --web-origin https://HOST:PORT
--web-cert CERT_PEM --web-key KEY_PEM --web-token TOKEN_FILE
--web-role administrator
```

同时发布 HAP 时，在上述 TLS 参数之外追加 `--hap-listen PRIVATE_IP:PORT`，并可重复添加 `--hap-scene SCENE` 选择场景。HAP 与 Web 须使用不同端点；HAP 允许明确的私网或回环地址，回环不发布 mDNS。只给 HAP 参数而不提供完整 Web TLS 参数会被拒绝，未选场景时只发布灯光。可用 `python3 tools/verify_web_tls.py /path/to/homesensed --tcp-combined` 在本机假网关验证完整 CLI，不连接现场。

这五项连接／凭据参数须成组提供；角色默认 `member`，配置发布需显式 `administrator`。令牌与私钥文件不得向组或其他用户开放，证书须由客户端信任。原有 `--coexistence-confirmed`、限时运行和已审核配置要求仍适用。此命令连接真实网关并可能执行启用规则。

管理监听就绪后才启动规则；配置发布先暂停视图并备份，发布或断线恢复后须完成反馈查询才能恢复控制。恢复不重放启动规则或旧命令。选定场景须已审核，且在候选配置中继续存在。

## Keypad 父对象候选

每块已核实六键 TS6-86 使用一个稳定 `keypads` 父对象，键事件、LED 与面板参数是子功能。旧逐键 ID 嵌入 `keys[].led_endpoint`，继续用于规则、权限、审计和 HAP；原生清单与 WSS 不重复发布这些子端点。六键固定顺序 1–2／3–4／5–6；接口及授权约束见 [原生 Keypad 契约](docs/keypad-native-api.md)。

候选支持显式 LED 红／绿／灭，以及背光、声音和工作模式的 typed 命令，全部共用准入、幂等收据、审计和唯一传输所有者。整板参数要求管理员完整覆盖六个已启用子端点；硬件模式必须与已发布配置和 LED 联动门禁一致。 新参数及原生 LED API 另外绑定精确源凭证／会话 generation，在入队、派发与实际写入前检查；同 controller 的其他凭证不能续用旧授权。新参数和 LED 精确来源租约的确定性回归已通过本机检查；普通控制与后续输出的精确来源授权仍在修复验证。按键动作只来自已审核的场景切换或已校准窗帘开／停／关映射。按下／释放、已受理、Written 与实际读回分别展示；没有核实读回格式的参数和 LED 实际值保持未知。

`tools/migrate_keypad_objects.py CONFIG REVIEW OUTPUT` 只按显式审核的模块与显示信息生成离线表示候选，不改版本、不写运行库、不发布、不设置硬件模式；已有别名、逐键启用范围及物理模块声明保留，未知型号或额外旧字段须人工处理。该候选尚未部署现场，原生卡片已实现并完成隔离接口联调，完整新候选现场验收仍待接续，EA-5 控制范围保持不变。

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

上述工具不连接真实网关；`check-automation` 只校验结构、引用及启用规则的命令编码，`study-replay` 只输出计划。工作区集成测试需要允许本机回环监听。配置发布取消与 HAP 故障隔离测试使用合成 TCP 网关和独立临时身份目录。

## 模拟与现场入口

本机模拟 HAP 服务使用内存网关，只监听 `127.0.0.1`，不发布 mDNS。以下运行 60 秒，状态与身份保存在独立目录：

```sh
cargo run --locked --offline -p homesensed -- serve-simulation config/simulation-home.json /tmp/homesense-simulation 51826 60
```

`serve-simulation-lan FILE STATE_DIR PRIVATE_IP:PORT SECONDS` 可在指定私网地址发布 mDNS，供 Apple 家庭模拟联调；仍不连接配置中的真实网关。服务时长上限为 24 小时，重启保留身份和配对，已有状态目录要求配置与监听端口匹配。

现场工具包含 `observe` 被动采集、`query-status` 单通道查询、`query-parameter` 参数查询、`motor-test` 开／关及 STOP 校准、`panel-led` 手动 LED，以及 `serve-live` 限时灯光桥。真实入口使用现场配置及显式确认参数，具体操作与证据见 [nanopc 部署记录](deployment/nanopc/README.md)；限定场景入口见 [阶段 D 场景迁移](docs/stage-d-study.md)。配置变更通过 `publish-config FILE STATE_DIR EXPECTED_REVISION` 在停止服务后发布；已有自动化的版本使用 `publish-automation DEVICE_FILE AUTOMATION_FILE STATE_DIR EXPECTED_REVISION` 同时发布两个文档。先备份运行库；命令要求服务停止，完成后也保持停止，不自动启用规则。

`config/import/` 为禁用导入草案，`config/live/` 为限定现场配置，`config/scenes/` 为书房迁移与调试资料。数据库升级与回退须使用匹配的程序和备份，并保留身份库中的最新配对及撤销状态。

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

- [完整设计](docs/design.md)：系统边界、协议契约、迁移方案及阶段 A–G 计划。
- [家庭网络架构设计](docs/network-architecture.md)：网络分区、双栈出口和设备迁移。
- [核心、QNAP 与 AC 配置步骤](docs/network-configuration-runbook.md)：DHCP、IPv6／PD、网络迁移与验收步骤；既有网络的核查及暂停点见对应文档。
- [更新记录](CHANGELOG.md)：逐次提交的变动细节、验证结果与历史检查点。
- [A–C 验收跟踪](docs/stages-a-c.md) 与 [逐项证据](docs/evidence/stages-a-c-requirements-20261011.md)：限定验收结论及未关闭问题。
- [阶段 D 完成审计](docs/stage-d-progress.md)：当前实现、验收缺口和增量验证记录。
- [书房场景迁移](docs/stage-d-study.md)：源规则、离线回放及手动场景现场证据。
- [实施记录](docs/implementation.md)：实现过程、测试与历史待办；当前阶段结论以上述审计为准。
- [nanopc 部署记录](deployment/nanopc/README.md)：候选版本、隔离测试、身份与回退边界；实时运行状态须另查进程。
- [协议核对记录](reference/control4/hb-protocol-notes.txt)：厂家报文依据和未验证事项。
- `reference/control4/`：Control4 解析器、HopeBeauty 地址映射和协议核对笔记；私有解析 JSON 与报告仅在本地保留。第三方手册、提取文本和原页截图已移出版本管理。
- `reference/manifest.json`：资料来源与哈希记录；`removed_third_party_documents` 记录已移除的第三方资料。保留范围与资料管理方式见 [参考资料说明](reference/README.md)。

原始项目仍在 `/Volumes/m4work/aiml/Control4/Projects/Charlies250801.c4p`；参考目录是离线快照，不是运行配置。完整解析 JSON/报告包含私人住宅资料，默认列入 `.gitignore`，发布前需另行审阅。旧资料保持原位，复制时逐文件核对哈希。

## 协议边界

普通帧为固定 5 字节二进制 `AE ID CMD VALUE EE`，无文档定义的校验和。地址按两位十六进制解析；禁止广播地址 00；亮度是整数 0..100。按注册模块类型区分开关反馈与面板按键，不为未知帧产生设备事件。面板 LED 依赖已处于手动模式，库不修改模块模式。

帧接收器支持拆包、粘包和逐字节恢复，内部至多保留四字节；payload 内的 AE/EE 不作为分隔符。无校验和意味着损坏数据偶然匹配起止字节时不能保证识别，后续语义分类是必要约束。断链应调用 `reset()` 丢弃残帧。

窗帘提供开／关／停，逻辑方向通过逐通道校准映射到电机升／降／停。可选行程配置用于明确标注为估计的运动、位置及百分比；未知起点不猜百分比，停止键活动提示与按钮可用性分开，主动命令不受估计终态或灯色抑制。估计与 LED 联动已有书房 HAP／iPad Web 的限定现场证据，完整范围仍待验。书房两个方向及 STOP 已有限定验收证据，其他通道仍须独立校准。命令接纳或写出不等于设备已完成动作；灯光状态以新鲜实际反馈为准，未知、过期或断线时报告不可用。

项目中的品牌全称统一为 `HopeBeauty`，代码与模块缩写使用 `hb`。原始 Control4 驱动标识、厂家资料路径、手册摘录和归档设计保留来源原文，解析器仍按原始驱动标识识别设备。
