# 文档导航

## 使用与维护

- [项目功能与快速开始](../README.md)
- [原生 App 使用、配置管理与本地调试](../apps/HomeSense/README.md)
- [系统设计](design.md) · [实现与本机验证](implementation.md)
- [NanoPi 主服务与冷备恢复](../deployment/nanopi/README.md) · [Release 构建](../deployment/ngr/RELEASE.md)
- [定时备份](../deployment/nanopi/backup/README.md) · [完整状态回退](state-upgrade-rollback.md)
- [网络架构](network-architecture.md) · [网络配置手册](network-configuration-runbook.md)
- [接管结项与长期观察](control4-takeover-checklist.md)

## 功能与接口契约

| 主题 | 文档 |
| --- | --- |
| 灯光 | [组合逻辑灯](composed-lights.md)、[共享通道](shared-lights.md)、[可用状态](shared-light-readiness.md) |
| 面板 | [设备模型](keypad-device-model.md)、[原生 API](keypad-native-api.md)、[统一动作](keypad-actions.md)、[窗帘按键](native-keypad-motor-press.md) |
| 窗帘 | [统一操作](curtain-migration-policy.md)、[状态与估计](curtain-state.md)、[源规则导入](motor-rule-import.md) |
| 场景 | [公共入口](scene-presentations.md)、[目录合并](scene-catalog-consolidation.md)、[房间归属](scene-placement.md)、[推断状态](scene-state-inference-policy.md)、[组件活动](scene-component-activity.md) |
| 全屋 | [HAP 全开／全关](hap-room-all-actions.md)、[离家执行](away-module-off.md)、[面板绑定](keypad-away.md) |
| 状态 | [查询与推送](adaptive-query-refresh.md)、[状态与 LED](state-and-indicator-policy.md)、[故障可用性](runtime-availability.md) |
| 管理 | [模块注册表](rs485-module-registry.md)、[精确来源授权](source-authorization.md)、[请求收据](web-receipt-policy.md)、[生命周期](service-lifecycle.md) |
| HAP | [原生 HAP 决策](adr/0001-native-hap.md)、[房间分配接口](homekit-room-placement.md)、[条件配方](hap-reviewed-program-actions.md) |
| 来源 | [Control4 语义审核](control4-source-semantics.md)、[规则节点审计](rule-source-audit.md)、[指示灯审核](control4-indicator-audit.md) |
| 通道 | [灯光与窗帘清单](lighting-curtain-channel-map.md) |

## 历史档案

[CHANGELOG](../CHANGELOG.md)、[App 更新记录](../apps/HomeSense/CHANGELOG.md)、`archive/` 和 `evidence/` 用于追溯历史决定、失败、测试与验收范围，不作为当前操作步骤。历史未勾选项不自动成为当前待办，也不批量改写为通过。

阶段记录：[A–C](archive/stages-a-c.md)、[D 书房](archive/stage-d-study.md)、[D 进度](archive/stage-d-progress.md)、[E 进度](archive/stage-e-progress.md)、[E 余项](archive/stage-e-remaining.md)、[D/E 审计](archive/de-completion-audit.md)。部署记录：[NanoPC 开发部署](archive/nanopc-development-deployment.md)、[NGR 阶段 E](archive/ngr-stage-e-deployment.md)、[NGR 双网模拟](archive/ngr-dual-vlan-simulation.md)。

原始现场配置、配对、凭证、日志与私有验收包不在公开文档中分发。
