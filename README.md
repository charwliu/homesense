# HomeSense · 知居

<img src="docs/assets/homesense-logo-v3.png" alt="HomeSense 知居 Logo：橙色家庭大脑连接琥珀色照明、蓝色自动化与绿色节能节点" width="160">

HomeSense（中文名：知居）是原生 Rust 家庭中控，通过 TCP485 网关控制 RS485 模块，并通过原生 Rust HAP 接入 Apple 家庭。目标是逐步替代现有的Control4 EA-5 的灯光、窗帘、场景与事件逻辑。

## 主要功能与实现情况

| 功能 | 能力 | 实现情况 |
| --- | --- | --- |
| 灯光控制 | 开关、调光、独立供电组合灯；Web 滑动条、关灯后恢复亮度 | 已实现；外部调光后的实际亮度发布仍在修复 |
| 场景与自动化 | 场景开关、互斥选择、取消、变量、条件、手势／长按、日历、延迟与渐变；未知状态显式翻转和反馈对账 | 运行时已接入，限定范围已有现场验证；物理推断场景的正向 LED 同步仍在修复，原项目其余语义待审核 |
| RS485 模块管理 | 独立模块状态、整模块／逐路查询、稳定 `module_id + channel` 引用、组合设备和模块换址 | 软件已实现；实际型号能力、换址及持续反馈仍待现场验收 |
| Apple 家庭 | 原生 Rust HAP，灯光、选定场景、窗帘开／关／停、可选面板事件、保留配对与多桥 | 已实现；当前版本真实客户端同步与恢复验收仍在推进 |
| Web 管理 | 设备、房间、场景、校准、权限、草稿／预览／发布、历史恢复与备份检查 | 已实现；当前版本完整终端交互与事件恢复仍待验收 |
| 状态与恢复 | 物理反馈与控制目标分离；启动／重连查询、周期刷新、配置代次隔离、断线不重放旧动作 | 软件已实现；当前实际总线的恢复与持续运行验证仍在推进 |
| 协议与迁移 | HopeBeauty 二进制协议、型号能力校验、Control4 导入与离线回放、命令及规则来源审计 | 已实现协议和导入框架；完整项目迁移尚未完成，未审核候选保持禁用 |

功能实现、软件验证与现场验收的范围见 [D/E 完成审计](docs/de-completion-audit.md) 和 [阶段 E 剩余清单](docs/stage-e-remaining.md)。逐次提交的实现细节与验证记录见 [CHANGELOG](CHANGELOG.md)。

## HAP 与 Web 组合模拟

```text
homesensed serve-combined-simulation DEVICE_FILE AUTOMATION_FILE STATE_DIR SECONDS HAP_LOOPBACK_IP:PORT WEB_LOOPBACK_IP:PORT TOKEN_FILE SCENE...
```

服务先确认 HAP 端口绑定和初始发现记录就绪，再启动自动化；端口占用不会执行启动规则。两个地址必须是不同的本机回环端点；令牌文件权限须为 0600，Web 角色为 member。该命令使用模拟硬件，在同一运行时发布灯光与指定场景，同时提供 Web 灯光和场景控制。它不支持配置激活；现有 HTTPS/WSS 与 TCP 管理入口不受影响。可执行 `python3 tools/verify_web_simulation.py /path/to/homesensed --combined` 验证会话、重启幂等及错误参数拒绝。

## TCP 服务的 Web 管理

`serve-automation-tcp` 可附加以下参数，启用与总线运行时共用准入、状态和发布监督的 HTTPS/WSS 管理入口：

```text
--web-listen LAN_IP:PORT --web-origin https://HOST:PORT
--web-cert CERT_PEM --web-key KEY_PEM --web-token TOKEN_FILE
--web-role administrator
```

同时发布 HAP 时，在上述 TLS 参数之外追加 `--hap-listen PRIVATE_IP:PORT`，并可重复添加 `--hap-scene SCENE` 选择场景。HAP 与 Web 须使用不同端点；HAP 允许明确的私网或回环地址，回环不发布 mDNS。只给 HAP 参数而不提供完整 Web TLS 参数会被拒绝，未选场景时只发布灯光。可用 `python3 tools/verify_web_tls.py /path/to/homesensed --tcp-combined` 在本机假网关验证完整 CLI，不连接现场。

这五项连接／凭据参数须成组提供；角色默认 `member`，配置发布需显式 `administrator`。令牌与私钥文件不得向组或其他用户开放，证书须由客户端信任。原有 `--coexistence-confirmed`、限时运行和已审核配置要求仍适用。此命令连接真实网关并可能执行启用规则。

管理监听就绪后才启动规则；配置发布先暂停视图并备份，发布或断线恢复后须完成反馈查询才能恢复控制。恢复不重放启动规则或旧命令。选定场景须已审核，且在候选配置中继续存在。

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

窗帘只提供开／关／停，不推算位置百分比；逻辑方向通过逐通道校准映射到电机升／降／停。书房两个方向及 STOP 已有限定验收证据，其他通道仍须独立校准。命令接纳或写出不等于设备已完成动作；灯光状态以新鲜实际反馈为准，未知、过期或断线时报告不可用。

项目中的品牌全称统一为 `HopeBeauty`，代码与模块缩写使用 `hb`。原始 Control4 驱动标识、厂家资料路径、手册摘录和归档设计保留来源原文，解析器仍按原始驱动标识识别设备。
