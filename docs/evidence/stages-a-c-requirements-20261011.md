# A–C 要求与证据核对（2026-10-11）

范围依据：design.md 第 20、28 节。以下通过项仅覆盖所列要求；不能外推为 EA-5 全量替代或正式部署完成。

| 要求 | 当前可核查证据 | 判定 |
| --- | --- | --- |
| A1 手册向量、边界、分片／粘包和未知帧 | hb-protocol 单元测试及 tests/profiles.rs；R8I8、AD8、C4、TS6 厂家向量；工作区日志 | 已验证 |
| A2 模拟反馈、订阅、失联不重放、STOP 容量 | homesense-core 队列／执行测试，homesensed feedback、drive、recovery、admission 测试 | 已验证 |
| A2 发布／存储故障屏障和白名单 | arbiter.rs 的存储故障、发布 STOP 排空；Admission 持久取消、错误恢复、旧代际拒绝 | 已验证；不包含完整在线 HAP 图热替换 |
| A3 iPhone 配对、虚拟灯读写／反馈、重启身份 | nanopc/20261011-a9.json；a9/server-http.log、server-restart.log；用户操作确认；原身份摘要相同 | 已验证指定序列 |
| A4 AID/IID、stale、权限、多客户端 | homesense-hap storage、observed、http_loopback；vendor/hap 单会话溢出隔离／撤销测试 | 已验证模拟及加密回环；HAP 依赖 52 项单独计数 |
| A5 136 对象、68 场景、215 程序、全部引用处置 | a22-import-bundle/coverage-report.json，3767 引用；configuration.json 全禁用；confirmation-checklist.json | 已验证原始 C4P 导出 |
| A5 独立三文件、源指纹、不丢未知节点、房间阻塞项 | importer CLI export 和 room 回归；study-blockers.json；未知归属保留全局复核；程序树保留 ID／容器路径 | 已验证；未实现程序执行（阶段 D） |
| A6／B 端点、型号、允许查询、回滚和真实延迟／反馈 | stages-a-c.md 现场边界／操作单；nanopc/README.md；a21/readonly-*-bus.json 与 service.log | 已验证限定书房查询与代理断线／丢回复恢复；不等于物理网关掉电验收 |
| C 开关与调光 | a21/field-result.json，50→30→关→单开恢复 30，用户确认跟随且无回跳 | 指定序列通过 |
| C 手动 LED | a20/panel-key2-field-result.json，41/2 绿灯 60 秒恢复红灯 | 指定单键通过；没有逐键颜色读取能力 |
| C 电机方向及 STOP | a22-motor/open-close-3s.json、drain-field-result.json 及 pcap；Rust 准入／持久审计／唯一写入路径 | 连续开关、两方向中途停止已确认；无位置百分比校准 |
| 最新版本集成 | 最新完整工作区 230 项通过、0 失败、0 忽略；日志 stages-a-c-workspace-20261011.log，哈希见 audit JSON | 已验证最新导入改动 |

## 尚未关闭的问题

1. 历史窗帘无明显动作轮次未同步抓包，无法确定发生于 TCP、网关串口、模块或电机。成功复测不能证明故障原因已消除。STOP 后半关闭修复已现场复测，但两轮无剩余反馈，未读反馈路径仅由回环验证。
2. 主灯曾发生两次查询超时并自动恢复，后续采样未重现。a21 丢回复注入证明恢复行为，不证明原超时根因。
最新完整回归已覆盖导入改动；HAP 依赖的独立测试不重复计入工作区数量。

正式 mDNS 部署、systemd、72 小时运行（F）、自动化程序执行（D）、全屋接管（G）保持原阶段归属。上述异常仍保留在交付风险中，不因阶段归属而删除。最终核定：设计第 20、28 节规定的 A–C 交付物与明确验收序列已通过，阶段目标完成。历史异常作为未关闭问题保留；本结论不声称根因已解决、无人值守可靠性已验收或已完成阶段 F。
