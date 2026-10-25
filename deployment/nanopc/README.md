# NanoPC 网络边界维护

NanoPC 承担 VLAN4／WAN B 网络边界，HomeSense 主服务运行在独立 NanoPi R6S。网络端口、地址服务与隔离规则见[网络架构](../../docs/network-architecture.md)和[配置手册](../../docs/network-configuration-runbook.md)。

HomeSense 的本机 Release 构建、部署与恢复使用[NanoPi 指南](../nanopi/README.md)。完整测试与模拟回归在开发本机执行；部署目标仅进行必要的兼容性、启动、健康与只读验收。

开发阶段的部署记录见[历史档案](../../docs/archive/nanopc-development-deployment.md)，不作为当前部署步骤。
