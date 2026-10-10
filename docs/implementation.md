# HomeSense 实施状态

日期：2026-10-10。位置：`/Users/liuwei/works/homesense`。

阶段 A1 的实现包括：

- 核心地址、通道、亮度、稳定设备 ID、模块类型和命令/观测类型；地址 00 禁止，地址字符串严格按两位十六进制处理。
- 按厂家版本定义开关、4/8 通道调光、电机和 BT-86 六键面板配置；重复地址拒绝且不覆盖已有注册。面板键数从型号推导，未确认型号只保留原始报文。
- 手册中的开关、调光、查询、暂停渐变、电机升降停和面板 LED 编码；不支持的模块/命令组合返回错误。
- 注册类型驱动的反馈分类、未知报文保留；电机不提供虚构的位置反馈。
- 固定 5 字节流解码、分片合并、噪声恢复、残帧清除与诊断计数。
- 离线 demo 和 hex decode 入口；没有 socket、规则执行或部署单元。

workspace 最低 Rust 版本为 1.99，所有 crate 继承该要求。本机默认工具链已升级到 Rust/Cargo 1.99.0（aarch64-apple-darwin）。全目标离线构建、81 个单元／集成测试、格式检查和 Clippy `-D warnings` 均通过，离线 demo 的编码及模拟反馈正常。十六进制解析已采用 `is_multiple_of` 和 `as_chunks`，满足新版 Clippy 要求。

单元测试覆盖厂家报文向量、范围与能力限制、重复地址、同帧不同模块含义、未知地址/命令/值、全部双段分片边界、逐字节输入、连续报文、起止字节作为 payload、错误恢复、断链丢弃和大量畸形输入下缓冲上限。离线向量验证不能代替硬件联调。

A2–A6 尚未完成：A2 已有状态、队列和模拟器基础，统一运行时仍待实现；HAP 配对与持久化、多配件桥、Control4 配置导入及只读现场联调尚未实现。A1 的注册数据目前由代码构造，未直接加载 136 个对象，也未把未核对的面板键数默认为 6。

## 设计 v1.7 核查后的待办

A1 型号及验证门槛已落地；后续运行时要求继续跟踪如下：

- A1 已实现：ProtocolProfile、VerificationRecord、Pending／Simulated／Verified 和 LED 模式检查。真实编码／分类默认拒绝模拟设备；模拟路径需显式选择。新增 5 个集成测试覆盖六键按下／释放、LED、D7–E0 越界、pending 拒绝、模拟与现场隔离、模式和验证记录。现场证据由操作人员提供，当前未登记任何真实设备验证。
- A2：实现全局配置发布屏障和 control_epoch 失效机制；取消旧场景、规则、定时器及未发送普通命令，待发 STOP 显式接管并按原映射排空。发布失败不复活旧任务；成员已写出时保留 partial／uncertain。模拟存储提交失败与迟到任务竞争。
- A2 及后续存储／接入集成：实现默认关闭的手动降级选项。存储故障时 HTTP 写入统一拒绝；已有授权 HAP 和验证面板仅在满足条件时执行灯光绝对立即动作与电机 STOP。恢复需验证存储、revision 和状态，并取消旧代际动作。实际 SQLite、HAP、场景及规则接入后补充对应集成测试。

## A2 离线进展

当前 workspace 有 9 个 crate、81 个测试，`cargo test --workspace --offline`、格式化及 Clippy `-D warnings` 通过。

- `homesense-state`：desired 与 observed 分离；写出不确认状态；重复反馈刷新时间，断链保留观测值但标记 stale；旧连接反馈拒绝；有界事件历史丢失后要求重取快照。
- `homesense-core::queue`：有界容量、STOP 预留、同设备顺序、立即亮度合并、超时及三类代际检查。STOP 取消待发升降，配置发布可提取 STOP 供显式校验排空。
- `homesense-config`：JSON schema v1 严格字段解析，检查 gateway／device ID、重复端点、十六进制地址、通道冲突、型号、验证证据及启用状态。设备默认禁用，未确认布局仅可保留禁用记录；模拟配置需显式 Simulation。SQLite 发布前与启动读取时校验，并核对 payload revision。`homesensed check-config FILE [--simulation]` 只读检查；样例位于 `config/simulation.json`。5 项配置测试、1 项存储集成测试及 1 项 CLI 测试通过。Control4 映射、场景引用、规则预算和完整运行时仍待接入。
- `homesense-storage`：SQLite schema v1、不可变配置 revision、活动指针和发布取消审计的单事务提交，期望 revision 冲突检测，WAL + FULL。6 项文件数据库测试覆盖重开、审计插入失败回滚、双连接冲突、外部数据库拒绝、不可变 revision、锁失败，以及仲裁器从实际数据库恢复未知提交结果。低层接口仍存储有界不透明字节；新增配置接口在发布与读取时校验类型、设备引用和 revision。命令幂等日志、备份及服务启动编排仍待接入；HAP 身份使用独立存储。依赖锁定 rusqlite 0.40.0，构建 bundled SQLite，首次构建需要依赖缓存。
- `homesense-core::arbiter`：拥有队列与准入策略，串行单帧写入；demo 已使用该入口。发布开始增加 control_epoch，取消旧普通命令，重新校验并以旧 revision 排空 STOP；在途帧阻止提交，失败不恢复队列，未知提交结果保持屏障，提交后等待显式同步。11 项集成测试覆盖成功、回滚、失败、超时、撤权、未知提交及存储故障／恢复。存储故障增加控制代际并取消旧队列；STOP 需作为新降级动作显式重验。恢复等待在途帧／STOP 排空，取消普通降级队列；数据库 revision 不匹配进入维护状态。配置发布事务已有 SQLite 集成测试；不可变设备映射安装、真实同步及定时器取消仍由未来运行时接入，单独的 core 测试不代表实际持久化。
- `homesense-core::admission`：入队及发送前共用策略检查；验证授权结果、设备存在、配置／连接／控制代际、控制权、Ready、deadline。存储降级默认拒绝；显式启用后只允许既有 HAP／直接面板绑定的已验证设备执行绝对灯光及电机 STOP，拒绝 HTTP、场景、规则、渐变和 LED。内部后台查询允许只读模式。测试输入是可信运行时夹具，实际身份鉴权、配置加载和持久化接受事务尚未实现。
- `homesense-core::execution`：单命令结果跟踪已接入 demo，区分 queued、writing、written、observed、timed_out、cancelled、failed 与 uncertain；反馈需匹配设备、连接代际、目标值及有效期。部分写入失败和写出后失效保留不确定性；不提供自动重试或持久化日志。查询结果关联及多命令协调仍待实现。
- `hb-transport`：单连接有界读写、经编码器检查的 PreparedFrame、短写处理、部分失败后关闭连接、连接代际检查、超时与 EOF 残帧处理；默认现场连接拒绝模拟帧。提供带连接／读写超时的 TCP 构造接口，不自动重连或重放。4 项内存故障测试及 1 项 localhost TCP 集成测试通过；沙箱阻止本地监听后，经批准在沙箱外复测通过。未连接现场网关。运行时鉴权、发布屏障与传输的整体集成仍待完成。
- `homesense-simulator`：根据实际收到的协议命令生成反馈；支持拆包、延迟、重复、丢回复、断链残帧清除。电机不生成位置或虚构 ACK；渐变直接达到目标，不模拟物理渐变时间。
- `homesensed demo [0..100]`：经过队列、编码、模拟网关、解码和状态存储；端到端测试覆盖 0、30、73、100 与非法输入。写出后 observed 仍为空；反馈达到绝对目标才结束 pending。协议没有事务编号，目标匹配不等于本次命令收到 ACK。

这些组件尚未组成具备授权和持久化的统一控制入口。发布屏障与实际存储／配置映射／定时器的集成、存储健康检测与降级切换的真实接入、超时结果与统一控制入口的集成、TCP actor 调度与现场连接验收仍待实现。未修改 EA-5，未执行现场控制。

## SQLite 实现依据

- [rusqlite 0.40.0 Transaction](https://docs.rs/rusqlite/0.40.0/rusqlite/struct.Transaction.html)：显式提交，未提交事务默认回滚。
- [SQLite synchronous](https://www.sqlite.org/pragma.html#pragma_synchronous)：存储连接使用 WAL 和 FULL；关闭重开测试不等同于断电测试。

数据库应放在服务专用受限目录。当前存储接口不保存配对密钥或凭据，也未实现完整的备份／权限部署流程。

配置解析按 [Serde 容器属性](https://serde.rs/container-attrs.html) 使用 `deny_unknown_fields`；验证状态采用结构变体，测试确保额外字段不会被忽略。

## A3 候选核查

固定 hap-rs 提交的编译及回调探针见 [ADR 0001](adr/0001-native-hap-candidate.md)。受控源码已置于 `vendor/hap`，通过 `homesense-hap` 接入主 workspace，未启动服务；主工程 81 项测试不包含候选探针。原生 Rust HAP 架构不变，候选已升级 dalek 依赖并分离读、意图写入、观测更新，9 项上游测试和 8 项探针通过；完整依赖审查、请求分组与 HTTP 入口的接线、身份存储与服务启动的整合及实机验证仍待完成。


`homesense-hap::storage::IdentityStore` 实现原生 HAP Storage trait：独立 SQLite 数据库，配置／配对／AID 缓存命名空间，WAL + FULL，阻塞任务执行 I/O，目录 0700、文件 0600，拒绝符号链接和过宽权限。不支持的非 Unix 平台显式拒绝打开。正常保存不能替换既有配件身份；损坏身份或缺少身份但保留配对记录时拒绝初始化。6 项存储测试验证重开、身份保护、命名空间／容量限制、权限、符号链接及锁冲突。

主 workspace 81 项测试和 Clippy 通过。上游仍有独立警告，vendor 未作为 workspace 成员执行同一 lint 门槛。`vendor/hap/UPSTREAM.json` 记录固定来源和打包差异，许可证保留。当前没有可启动的 HomeSense HAP 服务入口，未生成用户配对码或对局域网广播。

请求规范化已实现并通过 5 项测试：按设备合并同组 On/Brightness，关闭意图优先；仅开启时使用已确认非零亮度或配置默认值。拒绝重复特征、越界值和不支持的授权字段；订阅不产生控制动作。控制器身份由调用方可信会话传入，尚未接入 HAP HTTP 入口。仲裁器新增批量准入，整批校验成功才替换队列；后续成员失败不会留下部分入队或 STOP 取消副作用，2 项回归通过。该接口尚未与持久化接受事务整合。
