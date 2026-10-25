# HomeSense · 知居

<img src="docs/assets/homesense-logo-v3.png" alt="HomeSense 知居 Logo" width="160">

HomeSense 是用 Rust 编写的家庭中控系统，通过 TCP485 网关控制 HopeBeauty RS485 灯光、窗帘、风机与墙面板，通过 HAP 接入 Apple「家庭」，并提供 iPhone／iPad／Mac 原生 App 和 HTTPS/WSS 管理接口。

## 功能

- **灯光**：开关、直接百分比调光及关灯后的亮度恢复。供电与调光通道可合成为逻辑灯，共享通道按实际反馈协调，保留其他亮灯所需的供电。
- **窗帘**：打开、停止、关闭及目标百分比控制，支持同方向再次点按停止。位置按配置的行程时间估计，界面区分位置估计与实际反馈。
- **六键面板**：TS6-86 面板事件直接进入统一执行链，支持按下／释放处理、重复事件去重及跨面板联动。当前按键即时显示操作提示，联动 LED 在相关负载反馈确认后更新。
- **场景与自动化**：支持审核后的灯光配方、条件、变量、延迟、回家、离家与全屋开关。房间“全部”在有灯亮时执行关闭，全灭时执行开启；明确的全开／全关入口按各自配置执行。
- **全屋控制**：按模块发送并等待反馈，再推进下一模块。全屋关闭包含排风扇；失败模块进行有界重试并报告结果。停止与离家请求使用独立优先通道。
- **风机**：继电器风机提供独立开／关，Apple「家庭」使用标准风机类型，HomeSense 将其归入“温控”。
- **状态同步**：根据设备反馈更新灯光、场景与 LED 状态，通过 HomeKit 和 HTTPS/WSS 同步到客户端。实际状态、操作目标与待确认提示分别显示。
- **485状态采集**：R818 使用逐通道查询，不支持整模块状态查询；七个接入模块启用主动反馈，周期查询作为补偿。AD8 支持整模块状态读取。
- **自适应查询**：启动时同步设备状态，结合主动反馈和周期查询；扫描完成后再开始下一轮。周期按活动与超时退避在 300～7200 秒间调整，连续通信失败向 App 推送告警。
- **配置管理**：设备、房间、面板和场景配置支持草稿编辑、预览、版本校验及发布。访问凭证按设备、场景和管理能力授权，运行请求逐次校验权限。
- **运行恢复**：支持网关重连、状态重新同步、请求审计、备份与发布回退。中断的命令不会自动重放，部分写入保留不确定结果。

## 原生 App

家庭页面按类别和房间展示配件，支持常用配件、服务改名及 Apple「家庭」房间归属管理。

- 灯光详情使用竖向调光条：点击选择 10% 档位，拖动按 1% 调整，松手提交目标。
- 窗帘比例条支持左右拖动，并提供独立的打开／停止／关闭按钮。
- “房间排序”通过拖动调整家庭页面与房间列表顺序，支持恢复默认排列。
- 配件可单独隐藏，或按“灯带”名称前缀批量隐藏；支持单项恢复和全部恢复。
- 房间排序和隐藏偏好按家庭保存在本机，重开后保留，手机和平板可分别设置。

Apple「家庭」通过 HomeSense 桥使用逻辑灯、风机、多灯场景与原生窗帘。单灯使用独立灯光入口，墙键动作与 LED 联动由服务端统一执行。

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

本机模拟使用内存网关和独立状态目录，部分测试使用回环监听。生产部署使用本机交叉编译的 Release 程序，按目标架构上传；目标侧进行启动、健康与必要的只读验收。构建输出统一放在仓库根目录的 `target/`。

## 项目结构

| 路径 | 内容 |
| --- | --- |
| `crates/homesense-core`、`crates/homesense-state` | 设备、场景、规则及状态投影 |
| `crates/hb-protocol`、`crates/hb-transport` | HopeBeauty 帧协议与 TCP 传输 |
| `crates/homesense-config`、`crates/homesense-storage` | 配置校验与 SQLite 持久化 |
| `crates/homesense-hap`、`crates/homesensed` | Apple 家庭适配与服务入口 |
| `crates/homesense-import`、`crates/homesense-simulator` | Control4 导入与离线模拟 |
| `apps/HomeSense` | iPhone／iPad／Mac 原生客户端 |

## 文档

- [系统设计](docs/design.md) · [实现说明](docs/implementation.md)
- [原生 App](apps/HomeSense/README.md) · [配置管理](apps/HomeSense/README.md#完整配置草稿)
- [逻辑灯](docs/composed-lights.md) · [共享通道](docs/shared-lights.md) · [查询与推送](docs/adaptive-query-refresh.md)
- [Keypad 接口](docs/keypad-native-api.md) · [场景入口](docs/scene-presentations.md) · [组件活动](docs/scene-component-activity.md)
- [NanoPi 部署](deployment/nanopi/README.md) · [网络架构](docs/network-architecture.md) · [网络配置](docs/network-configuration-runbook.md)
- [接管结项清单](docs/control4-takeover-checklist.md) · [更新记录](CHANGELOG.md) · [参考资料](reference/README.md)
