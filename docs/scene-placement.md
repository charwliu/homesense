# 场景房间归属与发布

场景名称、房间与展示范围由显式元数据管理，不按名称、面板位置或动作目标自动猜测。归属编辑不改变动作、LED 关联、稳定场景 ID 或授权。

## 配置

`scene_metadata[]` 使用 `room` 与 `scope` 描述归属：

| scope | 含义 | room |
| --- | --- | --- |
| `room` | 房间场景 | 引用同版本的有效房间 |
| `household` | 全屋场景 | 不同时指定房间 |
| `unassigned` | 待分配 | 不同时指定房间 |

缺省且没有房间的记录保留为待分配。接口通过 `/api/v1/session` 的 `capabilities.scene_placement` 声明能力，客户端据此编辑；缺少能力时不发送不支持的字段。

公开场景与显式关联的内部配方同步维护归属。相同名称不合并不同 ID，跨房间配方的主要归属须审核，归属不扩大实际控制范围。

## 使用

在 App 配置管理编辑场景，保存草稿、预览后明确发布。发布前活动目录保持原归属。GET／WSS 提供 `room:{id,name,floor}|null` 和展示范围，按当前凭证过滤。

Apple「家庭」中的房间由 HomeKit 管理。服务器元数据不直接改变客户端房间，应使用[App 房间归属流程](../apps/HomeSense/README.md#apple-家庭房间归属)或在 Apple「家庭」编辑已有配件，保留原桥身份及配对。

## 离线整理工具

```sh
python3 tools/assign_scene_rooms.py   --project '<解析后的项目JSON>'   --devices '<当前设备配置JSON>'   --automation '<当前自动化JSON>'   --output '<新的私有输出目录>'
```

输出目录须不存在。工具不连接网关或发布配置，跨房间项需结合来源和明确决定核对后再使用；原始工程、逐项映射和配置留在私有目录。
