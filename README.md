# HomeSense · 知居

<img src="docs/assets/homesense-logo-v3.png" alt="HomeSense 知居 Logo：橙色家庭大脑连接琥珀色照明、蓝色自动化与绿色节能节点" width="160">

HomeSense（中文名：知居）是原生 Rust 家庭中控，通过 TCP485 网关控制 HopeBeauty RS485 模块，并通过原生 Rust HAP 接入 Apple 家庭。目标是逐步替代 Control4 EA-5 的灯光、窗帘、场景与事件逻辑。

## 当前实现

截至 2026-10-11，阶段 A–C 已完成设计规定的实现及限定范围验收，阶段 D 仍在进行。完整证据与未关闭问题见 [A–C 验收跟踪](docs/stages-a-c.md) 和 [阶段 D 完成审计](docs/stage-d-progress.md)。

- **协议、状态与控制**：二进制编解码、分片接收、型号与验证门槛、desired/observed 分离、有界队列、STOP 预留、持久化准入和唯一传输所有者；包含断线恢复、周期查询、配置发布及存储故障屏障。
- **Apple 家庭**：原生 HAP 桥与灯光配件，持久化身份、配对和 AID/IID；支持加密读写、订阅通知及反馈失效投影。已有 iPhone 配对、重启身份和书房主灯组合控制的限定现场证据。当前 HAP 图只提供灯光，窗帘通过独立逻辑控制入口执行。
- **Control4 导入**：读取 C4P／project.xml，保留源指纹、对象和程序树，生成运行配置、覆盖报告及人工确认清单。原项目 136 条设备映射、68 场景、215 程序均纳入处置；导出设备和候选规则默认禁用。
- **场景与规则**：已实现条件 AST、变量事务、场景顺序与延迟取消、手势、规则链和上海时间日历调度等组件。书房阅读／日常／节能三个场景已完成手动执行、重复关闭及取消的现场验证；通用服务装配、配置原子发布和全项目语义审核尚未完成，面板自动执行保持关闭。

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
cargo run --locked --offline -p homesensed -- demo 30
cargo run --locked --offline -p homesensed -- decode 'AE 15 D1 1E EE'
cargo run --locked --offline -p homesense-import --bin study-replay -- config/scenes/study.json 5 0
```

上述工具不连接真实网关；`check-automation` 只校验结构、引用及启用规则的命令编码，`study-replay` 只输出计划。工作区集成测试需要允许本机回环监听。

## 模拟与现场入口

本机模拟 HAP 服务使用内存网关，只监听 `127.0.0.1`，不发布 mDNS。以下运行 60 秒，状态与身份保存在独立目录：

```sh
cargo run --locked --offline -p homesensed -- serve-simulation config/simulation-home.json /tmp/homesense-simulation 51826 60
```

`serve-simulation-lan FILE STATE_DIR PRIVATE_IP:PORT SECONDS` 可在指定私网地址发布 mDNS，供 Apple 家庭模拟联调；仍不连接配置中的真实网关。服务时长上限为 24 小时，重启保留身份和配对，已有状态目录要求配置与监听端口匹配。

现场工具包含 `observe` 被动采集、`query-status` 单通道查询、`query-parameter` 参数查询、`motor-test` 开／关及 STOP 校准、`panel-led` 手动 LED，以及 `serve-live` 限时灯光桥。真实入口使用现场配置及显式确认参数，具体操作与证据见 [nanopc 部署记录](deployment/nanopc/README.md)；书房场景入口见 [阶段 D 书房迁移](docs/stage-d-study.md)。配置变更通过 `publish-config FILE STATE_DIR EXPECTED_REVISION` 在停止服务后发布。

`config/import/` 为禁用的导入草案，`config/live/` 为限定现场配置，`config/scenes/` 为书房迁移及调试资料。当前本地运行库为 schema 9，尚未部署到 nanopc 原配对运行库；升级与回退须匹配数据库版本，保持身份库中的最新配对和撤销状态。

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
