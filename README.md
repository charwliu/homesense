# HomeSense · 知居

<img src="docs/assets/homesense-logo-v3.png" alt="HomeSense 知居 Logo" width="160">

HomeSense 是用 Rust 编写的家庭中控项目：通过 TCP485 网关管理 HopeBeauty RS485 灯光、窗帘与墙面板，通过原生 HAP 接入 Apple 家庭，并提供 HomeSense App 与 HTTPS/WSS 管理接口。项目正逐步迁移现有 Control4 EA-5 的场景和自动化逻辑。

## 功能

- **设备控制**：灯光开关与调光、窗帘开／关／停、六键 Keypad 及 LED 提示。
- **场景与自动化**：经过审核的场景、条件、变量与延迟；配置发布与运行状态分离。
- **状态与恢复**：以设备反馈确认实际状态；支持组件确认及实际程序写出证明提供的只读场景活动、断线重连、周期查询、故障隔离与请求审计。
- **客户端**：Apple 家庭中的 HAP 配件，以及 iPhone／iPad／Mac 原生 App。浏览器管理页面已撤除，HTTPS API 与 WSS 仍供原生 App 使用。
- **迁移工具**：Control4 资料导入、离线回放、模拟网关和现场诊断工具。

功能分批发布，软件验证不等于现网部署或全屋验收。**阶段 D/E 尚未完成**；当前范围、已验证能力及剩余工作以 [D/E 完成审计](docs/de-completion-audit.md) 和 [阶段 E 剩余清单](docs/stage-e-remaining.md) 为准。逐次变更见 [更新记录](CHANGELOG.md)。

## 构建与本机模拟

需要 Rust 1.99 或更新版本。依赖由 `Cargo.lock` 锁定；首次获取依赖后，可给 Cargo 命令添加 `--offline` 使用本地缓存。

```sh
cargo fetch --locked
cargo build --locked --workspace
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings

cargo run --locked -p homesensed -- check-config config/simulation.json --simulation
cargo run --locked -p homesensed -- check-automation config/simulation.json config/automation.simulation.json --simulation
cargo run --locked -p homesensed -- serve-automation-simulation config/simulation.json config/automation.simulation.json /tmp/homesense-automation-demo 10
```

上述模拟命令使用内存网关和独立状态目录，不连接真实 RS485 网关；部分测试需要本机回环监听。真实网关命令、配置发布和回退请按部署记录执行，不要把模拟状态目录用于现网。

## 项目结构

| 路径 | 内容 |
| --- | --- |
| `crates/homesense-core`、`crates/homesense-state` | 设备、场景、规则及状态投影 |
| `crates/hb-protocol`、`crates/hb-transport` | HopeBeauty 帧协议与 TCP 传输 |
| `crates/homesense-config`、`crates/homesense-storage` | 配置校验与 SQLite 持久化 |
| `crates/homesense-hap`、`crates/homesensed` | Apple 家庭适配与服务入口 |
| `crates/homesense-import`、`crates/homesense-simulator` | Control4 导入与离线模拟 |
| `apps/HomeSense` | iPhone／iPad／Mac 原生客户端 |

## 文档入口

- [系统设计](docs/design.md) · [实现说明](docs/implementation.md) · [协议核对](reference/control4/hb-protocol-notes.txt)
- [阶段 A–C 验收](docs/stages-a-c.md) · [阶段 D 进度](docs/stage-d-progress.md) · [D/E 完成审计](docs/de-completion-audit.md)
- [原生 App](apps/HomeSense/README.md) · [Keypad 接口](docs/keypad-native-api.md) · [场景入口](docs/scene-presentations.md) · [组件活动](docs/scene-component-activity.md)
- [nanopc 部署记录](deployment/nanopc/README.md) · [网络架构](docs/network-architecture.md) · [网络配置手册](docs/network-configuration-runbook.md)
- [参考资料说明](reference/README.md) · [更新记录](CHANGELOG.md)

完整私有工程、凭证和配对数据应保存在忽略的私有目录；`reference/` 是离线参考资料，不是运行配置。

调光控制统一发送直接目标百分比：App、组合灯恢复及自动化入口不再输出硬件渐变指令；场景原有亮度和供电范围保持，明确修改的目标另行记录。场景详情可按授权查看所含灯光的实测开关与亮度，复用设备状态推送。

本轮直接亮度、场景灯光清单及 LED 发送窗口已完成[软件隔离验收](docs/evidence/stage-e-browser/direct-level-scene-lights-qualification.json)，尚未部署现网；隔离结果不替代家庭总线与现场验收。
