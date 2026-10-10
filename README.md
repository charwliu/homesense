# HomeSense

原生 Rust 家庭中控，用于逐步替代 Control4 EA-5 的 HopeBeauty RS485 控制功能，规划通过原生 Rust HAP 接入 Apple 家庭。

## 当前实现

阶段 A1：`homesense-core` 设备类型及基础范围校验、`hb-protocol` 编解码与反馈分类、`homesensed` 离线工具。已实现面板协议配置、验证状态与 LED 模式门槛，真实设备验证记录仍待现场填写。A2 已加入状态存储、有界命令队列和确定性网关模拟器。已加入 `hb-transport` TCP 基础接口并完成 localhost 测试，CLI 仍为离线工具；此版本未连接真实 TCP485、不启动网络服务、不发送设备动作；统一控制入口、HAP 已接入受控依赖和身份存储，服务入口尚未实现；场景和迁移器尚未实现。

```sh
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo run --offline -p homesensed -- check-config config/simulation.json --simulation
cargo run --offline -p homesensed -- demo
cargo run --offline -p homesensed -- decode 'AE 15 D1 1E EE'
```

使用 Rust edition 2024，最低 Rust 1.99；已在本机 Rust/Cargo 1.99.0 上完成构建和测试，验证记录见 `docs/implementation.md`。协议与核心类型保持标准库依赖；存储使用锁定的 rusqlite 0.40.0 和 bundled SQLite。当前缓存下可离线构建，新环境需要先获取 Cargo.lock 中的依赖。后台服务最终名称为 `homesensed.service`，目前没有部署或自启动配置。

## 资料

- [完整设计](docs/design.md)：v1.7，补充面板能力验证、配置发布取消与存储降级边界，包含原生 HAP、迁移和阶段 A 计划。
- [实施状态](docs/implementation.md)：已实现范围、测试和后续工作。
- [协议核对记录](reference/control4/hb-protocol-notes.txt)：厂家报文依据和未验证事项。
- `reference/control4/`：Control4 解析器、解析 JSON、报告、HopeBeauty 地址映射、手册文本与原页截图。
- `reference/manifest.json`：每份资料的来源、复制时 SHA-256 和当前 SHA-256。设计文档及协议笔记更新后的哈希单独记录，原始复制哈希继续保留。

原始项目仍在 `/Volumes/m4work/aiml/Control4/Projects/Charlies250801.c4p`；参考目录是离线快照，不是运行配置。完整解析 JSON/报告包含私人住宅资料，默认列入 `.gitignore`，发布前需另行审阅。旧资料保持原位，复制时逐文件核对哈希。

## 协议边界

普通帧为固定 5 字节二进制 `AE ID CMD VALUE EE`，无文档定义的校验和。地址按两位十六进制解析；禁止广播地址 00；亮度是整数 0..100。按注册模块类型区分开关反馈与面板按键，不为未知帧产生设备事件。面板 LED 依赖已处于手动模式，库不修改模块模式。

帧接收器支持拆包、粘包和逐字节恢复，内部至多保留四字节；payload 内的 AE/EE 不作为分隔符。无校验和意味着损坏数据偶然匹配起止字节时不能保证识别，后续语义分类是必要约束。断链应调用 `reset()` 丢弃残帧。

窗帘只编码电机升/降/停，不推算位置，也不假定升降对应开关方向。编码成功不等于设备收到或状态已确认。真实型号、反馈模式、接线方向与原系统行为仍需现场验收。

A2 进行中：已有 desired/observed 分离、连接代际检查、命令优先级、STOP 保留容量和拆包／延迟／丢包模拟。下一步补齐统一控制入口、超时结果跟踪、配置发布屏障及存储故障集成，再验证原生 HAP 虚拟调光灯。

项目中的品牌全称统一为 `HopeBeauty`，代码与模块缩写使用 `hb`。原始 Control4 驱动标识、厂家资料路径、手册摘录和归档设计保留来源原文，解析器仍按原始驱动标识识别设备。

阶段 A–C 的范围、当前证据和书房测试边界见 [阶段跟踪](docs/stages-a-c.md)。
