# 阶段 D：书房三个场景迁移

约束：不修改 EA-5 配置、连接、规则或控制范围。HomeSense 面板自动执行保持关闭；已有 EA-5 变量 411 不可直接读取，不从灯光状态猜测该变量。

## 已核对源规则

来源 Charlies250801.c4p，指纹见 config/scenes/study.json。面板 41／Control4 对象 63 的 release 事件 2、6、10 分别对应键 1、3、5：

| 键 | 场景 | 模式变量 411 | 03/4 | 07/6 | 11/5 | 12/4 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 阅读 10 | 1 | 开 | 关 | 0% | 100% |
| 3 | 日常 6 | 2 | 开 | 开 | 100% | 100% |
| 5 | 节能 8 | 3 | 关 | 开 | 17% | 0% |

再次按同键：变量设为 0，执行该场景 Deactivate；用户确认其实际语义是场景控制的书房灯全部关闭。LED 动作严格保留原程序顺序：阅读／日常先激活场景后更新 LED；节能先更新 LED 再激活场景。重复按键的 Deactivate 与 LED 顺序也分别保留。

## 本轮实现

- 类型化条件求值、SetMode／Scene／LED 动作与来源节点路径；变量 unknown 跳过整个分支。
- 三场景目标展开，代理 ID 映射保留；拒绝未知场景、越界目标及非审核映射；源 delay／rate 不偷换成总线报文参数。
- `study-replay` 离线入口无传输依赖，输出计划而不写总线；生成 next_mode 不代表已持久化或物理完成。

```sh
cargo run --locked --offline -p homesense-import --bin study-replay -- config/scenes/study.json 5 0
cargo run --locked --offline -p homesense-import --bin study-replay -- config/scenes/study.json 5 3
```

## 运行时执行、取消与持久化

`homesensed::study_runtime::StudyRuntime` 已接入 Admission 与 TransportOwner。`activate(key)` 显式选择场景；`toggle(key)` 使用本地持久模式，首次未知时拒绝执行，不猜测 EA-5 变量。构造时必须显式选择 `TimingPolicy::ModuleNative`，保留原始 rate，使用模块已配置的渐变，不承诺与 Control4 的 1000 单位时长完全相同。当前三个场景均无延迟；出现非零延迟会拒绝整批，不静默忽略。

命令以 `Origin::Rule`、运行 ID 和启动／传输／配置代际进入同一个持久化准入。模式值与整批命令的 Accepted 记录在同一 SQLite 事务提交，比较预期旧值以拒绝并发旧状态；失败不更新模式。该变量表示本地选择意图，不表示灯已达标，也不与 EA-5 私有变量同步。

`cancel()` 先持久化取消记录，再移除未发送项；写入中的或已写出项不撤销。替换运行先取消旧待发项；直接手动控制会取消该设备待发场景动作，其余设备继续。重启保留模式选择，但旧命令不重放。取消不自动把逻辑模式清零，不伪造物理关灯。

数据库升级到 schema 5，增加按配置版本隔离的 study_mode 表；旧版程序不能读取新版数据库。本地测试库和 nanopc 独立场景测试库使用 schema 5，原有 HAP 运行数据库未升级。

自动面板触发仍关闭，EA-5 配置、连接和控制范围均未改动。一次性手动场景程序已部署并完成三场景现场灯光／LED 验收（见下节）；常驻自动场景服务未开启。反馈派生的 scene active、通用延迟场景及全项目规则迁移仍属后续阶段 D 工作。

验证：场景专项 6 项通过；完整工作区 238 项通过、0 失败、0 忽略。覆盖持久事务回滚、取消失败回滚、手动覆盖、替换运行、重启不重放，以及通过 TransportOwner 的精确节能报文顺序。证据见 `evidence/stage-d-study/runtime-validation.json`。

## nanopc 手动场景验收（2026-10-11）

独立版本 `20261011-d1-study` 的 `study-scene` 已部署，保留原 current 链接。
执行方式：`study-scene STATE_DIR KEY --coexistence-confirmed`，KEY 仅限 1/3/5。
配置与迁移文档编入二进制；每次先查询 TS6 C7 按键反馈灯手动模式（与背光亮度定时分开），再通过
StudyRuntime → Admission → TransportOwner 写出。独立状态目录
`/users/alarm/apps/homesense/tests/study-scenes-d1` 使用 schema 5，不迁移现有 HAP 状态。

依次手动触发阅读、日常、节能，每轮 7 条命令写出，用户逐项确认灯光与场景键指示灯均符合。
最终保留节能场景。进程执行后退出，自动场景订阅未开启，EA-5 配置及控制范围未修改。
本轮未测试现场取消、重复按键关闭场景；17% 为命令目标，现场确认明显变暗。
详见 `evidence/stage-d-study/nanopc-d1.json`。

## 重复触发与运行取消现场验收（2026-10-11）

`20261011-d2-study` 扩展手动入口，支持可选动作 `activate`（默认）、`toggle`、
`cancel-before`、`cancel-after-one`。取消测试在确定的调度边界调用同一运行时的
`cancel()`，随后再次调度并要求队列为空；不是网络写入中途打断测试。

- 阅读、日常、节能均由用户先确认激活，再重复触发；各 5 条关闭指令，用户确认全部灯关闭及对应场景键变绿。
- 三场景分别发送前取消：每轮 0 条写出、7 条取消，用户确认灯光和指示灯未变化。
- 三场景分别首条写出后取消：每轮 1 条写出、6 条取消。用户确认灯光仍关闭、1/3 键绿而 5 键红，随后没有继续亮灯。继电器首条动作未独立测量。
- SQLite 只读审计确认六轮取消数量；旧启动的 written 按既有恢复策略转为 uncertain，不重放，取消记录保持 cancelled。
- 最后显式 toggle 恢复模式 0，用户确认主灯／筒灯全关、1/3/5 键全绿；测试进程退出。

场景专项回归 7 项通过，新增覆盖三个场景重复关闭和发送前取消。EA-5 控制范围不变，自动触发仍关闭，current 仍指向 a19。详见 `evidence/stage-d-study/nanopc-d2.json`。
