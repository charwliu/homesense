# HomeSense 实现与开发指南

HomeSense 是 Rust workspace，原生客户端位于 `apps/HomeSense`。功能结构见[系统设计](design.md)，客户端操作见[App README](../apps/HomeSense/README.md)。依赖版本由 `Cargo.lock` 锁定，最低 Rust 版本以根 `Cargo.toml` 为准。

## 模块职责

| crate | 职责 |
| --- | --- |
| `homesense-core` | 设备、命令、事件、规则与场景类型 |
| `hb-protocol` | 型号能力、五字节帧编码、流解码及反馈分类 |
| `hb-transport` | TCP 连接与有界发送 |
| `homesense-state` | 反馈状态、质量、版本与逻辑投影 |
| `homesense-config` | 设备／自动化配置解析与校验 |
| `homesense-storage` | SQLite 配置、身份、请求与审计持久化 |
| `homesense-hap` | HAP 配件、配对存储、状态投影和加密接口接线 |
| `homesense-import` | Control4 归档读取、来源追踪、显式审核转换 |
| `homesense-simulator` | 内存网关与离线模拟 |
| `homesensed` | CLI、Admission、场景执行、传输所有者、HTTPS／WSS 与服务生命周期 |

## 本机检查

在仓库根目录执行：

```sh
cargo fetch --locked
cargo fmt --all -- --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo run --locked -p homesensed -- check-config config/simulation.json --simulation
cargo run --locked -p homesensed -- check-automation config/simulation.json config/automation.simulation.json --simulation
cargo run --locked -p homesensed -- serve-automation-simulation config/simulation.json config/automation.simulation.json /tmp/homesense-automation-demo 10
```

输出统一复用根 `target/`。出现失败时先保留日志、复现并修复定向用例，再运行必要回归；测试结果须对应产品源码与制品。只改夹具时分别记录原失败、构造修正和新结果，不能删断言或单纯放宽超时获得通过。场景状态读取上限为 500ms。

全套测试、模拟器及回环场景验收在开发本机执行，测试制品不上传部署目标。App 构建与 Swift 联调说明见[客户端指南](../apps/HomeSense/README.md#构建与运行)。

## 配置与来源

设备配置用稳定模块引用组织物理通道；自动化文档定义场景、规则、变量、指示灯及发布元数据。可用 `check-config` 和 `check-automation` 离线校验，不能以 JSON 可解析代替型号、接线或来源审核。

Control4 导入器只读取归档并生成报告／禁用草稿。源指纹、准确节点和显式转换计划用于核对控制范围；启用、身份授权与现场校准分别检查。转换契约见 [REVIEWED-SCENES](../crates/homesense-import/REVIEWED-SCENES.md)、[来源语义](control4-source-semantics.md)和[规则审计](rule-source-audit.md)。

## 关键运行路径

1. HTTP、HAP、实体按键或受信规则提交类型化意图。
2. Admission 检查配置代次、来源、权限、状态证明和预算，持久化受理记录。
3. 灯光与场景规划展开为有界物理动作；组合灯和共享资源在准入时共同求解。
4. 唯一传输所有者按模块、因果屏障及线速发送，在实际写前重查租约。
5. 写出记录与实际设备反馈独立提交；反馈更新逻辑灯、场景及 LED 投影。
6. HAP 与管理读取共用已生成状态，WSS 向有权限的订阅者交付最新结果。

`accepted`、`writing`、`written`、已观察及不确定结果不能互相替代。响应丢失、撤权或取消不会自动重发控制；旧 boot／配置／连接代次的回复不能恢复当前状态。

## 协议与状态约束

- 物理地址严格使用两位十六进制，按登记型号解释反馈，不按报文字节猜测设备类型。
- R818 逐路查询；七个接入模块开启主动反馈，保留补偿轮询。AD8 使用整模块状态查询。
- 直接亮度设置以整数百分比编码；协议层的渐变能力与场景实际选择的直接设定分开。
- 窗帘位置基于成功写出的轨迹和独立行程参数估计；LED 无颜色实测时保留未知或最后写出来源。
- 查询、确认读取、周期扫描及控制发送共用总线调度；客户端读取不会创建新全屋扫描。

详见[状态与指示灯](state-and-indicator-policy.md)、[查询调度](adaptive-query-refresh.md)、[组合灯](composed-lights.md)、[按键 API](keypad-actions.md)。

## 发布与维护

在本机交叉编译所需生产程序，始终使用 Cargo `--release`。NGR 为 `x86_64-unknown-linux-gnu`，NanoPi 为 `aarch64-unknown-linux-gnu`；逐项记录源码指纹、profile、架构、大小和 SHA-256。

目标侧只执行已部署程序的启动、健康和必要只读验收。配置、配对及凭证按授权范围单独处理。发布使用完整冷备、独立恢复守护和失败回退，保留当前与最近一个可执行回退版本。

- [Release 构建](../deployment/ngr/RELEASE.md)
- [NanoPi 主机与冷备恢复](../deployment/nanopi/README.md)
- [完整状态回退](state-upgrade-rollback.md)
- [结项验收与长期观察](control4-takeover-checklist.md)
