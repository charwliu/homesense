# HomeSense · 知居

<img src="docs/assets/homesense-logo-v3.png" alt="HomeSense 知居 Logo" width="160">

HomeSense 是用 Rust 编写的家庭中控项目：通过 TCP485 网关管理 HopeBeauty RS485 灯光、窗帘与墙面板，通过原生 HAP 接入 Apple 家庭，并提供 HomeSense App 与 HTTPS/WSS 管理接口。项目正逐步迁移现有 Control4 EA-5 的场景和自动化逻辑。

## 功能

串口网关在灯光或 LED 写入中断连时，服务在完成故障日志审计后进入重连和重新同步，不重发旧命令；写入一部分的指令保留不确定结果。权限、日志存储及内部状态错误仍会停止控制。这项网络写入恢复修复尚未发布。

- **设备控制**：灯光开关与调光、窗帘开／关／停、六键 Keypad 及 LED 提示。
- **场景与自动化**：经过审核的场景、条件、变量与延迟；配置发布与运行状态分离。
- **状态与恢复**：以设备反馈确认实际状态；支持组件确认及实际程序写出证明提供的只读场景活动、断线重连、周期查询、故障隔离与请求审计。
- **客户端**：Apple 家庭中的 HAP 配件，以及 iPhone／iPad／Mac 原生 App。浏览器管理页面已撤除，HTTPS API 与 WSS 仍供原生 App 使用。
- **迁移工具**：Control4 资料导入、离线回放、模拟网关和现场诊断工具。
- **延期刷新与通信告警**：组件短时确认不再维持永久密集查询；静稳后退到长周期，超时递增退避，连续三轮失败向 App 推送告警。见[查询调度说明](docs/adaptive-query-refresh.md)。

功能分批发布，软件验证不等于现网部署或全屋验收。**阶段 D/E 尚未完成**；当前范围、已验证能力及剩余工作以 [D/E 完成审计](docs/de-completion-audit.md) 和 [阶段 E 剩余清单](docs/stage-e-remaining.md) 为准。逐次变更见 [更新记录](CHANGELOG.md)。

离家执行增强与刷新新策略已完成软件隔离回归并更新服务程序，设备配置与控制范围保持：离家最多三轮模块关闭、总预算5秒，依真实写后反馈确认结果并向App推送准确失败范围；普通周期限制在300～7200秒，有面板／App控制活动恢复300秒，不重叠启动未完成扫描。详见[离家执行说明](docs/away-module-off.md)及[查询调度说明](docs/adaptive-query-refresh.md)。

待发布的全屋确认预算调整：完整串行反馈允许最多10秒，收到本次操作全部输出的关闭反馈即立即完成；单模块回复仍最多1秒、发送票据2秒、最多三轮，不延长普通查询或扫描周期。5秒旧上限的失败记录保留，不作为10秒版本的通过记录。

离家确认／失败终态会主动唤醒状态推送，继续重验当前凭证和运行配置；六键与独立离家入口的回执共用执行器的重试预算。普通推送保活和300～7200秒总线扫描周期保持，完整接管进度见[接管清单](docs/control4-takeover-checklist.md)。

七个已验证R818模块的主动反馈配置继续保留，周期查询作为补偿。R818不支持整模块状态查询，仍逐通道读取；同一模块的连续应答可合并接收，不能把多通道回包误当作支持整模块查询。当前场景与故障隔离模拟在同模块整批回包前仅随机等待一次3～20毫秒，然后连续发送全部通道；不逐通道附加等待。9600/8N1下，八条五字节应答的连续传输时间约41.7毫秒，模拟器按此计时。

首次创建访问凭证时，会同时授权已启用面板直接绑定的场景入口，独立场景列表中隐藏的面板动作也可使用；不会因此显示隐藏场景卡片或授权未绑定的内部辅助场景。已有凭证及撤销状态保持，后续配置新增入口仍需单独审阅权限迁移。

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

后台 LED 扫描接近周期截止时，若无法保留完整发送窗口，只退掉尚未接纳的计划，后续扫描重新取得当前反馈证明；不把短窗口中的新指令接纳后再取消，也不延长或重放已经接纳的指令。此项新增修复仍在整合验收，尚未部署。

共享供电与复用调光列通过显式 [`shared_lights` 配置](docs/shared-lights.md)单独建模，不放宽原组合灯的独占供电约束。独立灯光操作根据实际供电判断另一盏灯是否亮着；复用阅读通道时保留阅读供电；开启另一盏公共供电灯时退出夜灯，显式关闭已熄灭的该灯保留夜灯。未变化的其他调光与公共供电不重复写入，避免取消仍有效的场景状态。命令准入要求六路当前反馈和完整资源权限，供电关闭时的残留调光值不覆盖灯光恢复亮度。设备入口、六键场景状态和LED已通过本机联合回环检查；场景选择仍需实际写入和反馈证明，Linux及现场验收尚未完成，未发布。

物理 LED 随动可显式声明完整的开／关组合：`lights_off` 指定既有输入中必须关闭的部分，其余输入必须开启。颜色证明同时核对输入及开关要求，未知反馈不提前变色；既有按键动作和查询保持不变。见[配置与边界](docs/shared-lights.md)。此扩展仍在隔离验收，尚未部署。

## 完整配置草稿容量（候选，未发布）

完整设备／自动化草稿的保存通道支持最多4 MiB，原生 App 可重新读取和预览完整草稿；保存与预览仍不发布配置。普通控制、小项编辑和 WebSocket 保留原容量及权限限制，详见 [App 配置管理](apps/HomeSense/README.md#完整配置草稿)。

## 共享通道逻辑灯（候选）

原生灯卡片可通过 [`composed_lights`](docs/composed-lights.md) 组合共享供电与调光通道，保留物理绑定供查询、场景和诊断使用。重复的供电＋调光组合会被拒绝；单灯关闭不能切断其他已亮灯所需的共享通道，无法独立关闭时禁用相应操作。逻辑灯分别按实际反馈记忆亮度，场景互斥仍由场景配置负责。此功能未部署，私有接线候选仍需现场核对。
