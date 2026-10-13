# RS485 模块注册表与稳定引用

此检查点实现引用层、配置草稿和管理入口。可变通道状态仍由现有 Feedback 保存，管理页提供只读视图；独立于逻辑绑定的 canonical module-channel 状态拥有者尚未完成。未绑定通道反馈仍可能丢弃，见[独立反例](evidence/stage-e-browser/module-channel-truth-gap-root-check.json)。这一限制不能用绑定通道回归或模块页面通过来替代验收。

## 配置 schema 2

模块在网关的 `modules` 中保存不可变逻辑 `id`。`address`、网关位置、`profile` 和审核资料属于物理注册表映射；模块 ID 不从当前地址、型号或网关名称重新生成。

```json
{
  "schema_version": 2,
  "revision": 2,
  "gateways": [{
    "id": "synthetic-bus",
    "endpoint": "127.0.0.1:50007",
    "modules": [
      {"id": "module-dimmer", "address": "15", "profile": "ad8_din_rs485_v2_2_1", "verification": {"status": "simulated"}},
      {"id": "module-supply", "address": "03", "profile": "r8i8_din_pro_v2_7_7", "verification": {"status": "simulated"}}
    ]
  }],
  "devices": [
    {"id": "supply-binding", "name": "Synthetic supply", "module_id": "module-supply", "channel": 1, "enabled": true},
    {"id": "logical-light", "name": "Synthetic light", "module_id": "module-dimmer", "channel": 1, "supply": {"module_id": "module-supply", "channel": 1}, "default_on_level": 50, "enabled": true}
  ]
}
```

这些是模拟示例。逻辑灯不保存真实地址：调光引用自己的模块通道，供电引用第二个模块通道。现有运行时在受校验的注册表解析后得到物理绑定，再进入统一 Admission／TransportOwner；供电绑定在兼容适配层解析为原有隐藏 DeviceId，只有一条可控逻辑灯。场景、规则和 HomeKit 身份继续使用逻辑设备 ID。

配置解析拒绝重复模块 ID、重复物理地址／通道、未知模块引用、无效通道、混用旧物理绑定字段和新引用，以及不兼容供电类型。`ModuleSpec` 表示协议能力与审核，不表示模块在线或已执行命令。

## 旧配置迁移与换址

schema 1 可继续读取和使用原有逻辑控制。模块管理操作要求先显式迁移：基于已保存且不可变的私有草稿生成独立 UUID 模块 ID，把所有设备（含禁用绑定和隐藏供电）改为模块 ID／通道，并转换组合灯供电引用。生成的 schema 2 文档与 ID 一起保存；重读、重启、重连和换址只复用已保存 ID。迁移不改场景内容、逻辑设备 ID、名称、房间、现有校准或物理线路。

管理员路由：

- `POST /api/v1/config/drafts/{source}/module-migration`：请求含新草稿 `id`、字符串 `expected_revision`、`revision`。
- `POST /api/v1/config/drafts/{source}/module-replacement`：新增 `module_id`、新物理 `new_address`、`profile` 和新 `verification`。没有客户端旧地址目标。
- `POST /api/v1/config/drafts/{source}/module-binding`：用 `device`、`module_id`、`channel` 和 `enabled` 修改引用；变更绑定清除旧电机校准。
- `POST /api/v1/config/drafts/{source}/module-calibration`：针对准确模块 ID／通道录入已有校准证据，不执行测试。

替换只修改模块映射，保持逻辑引用；物理地址冲突、广播地址、类型或通道容量不兼容及能力校验失败均拒绝。地址绑定的电机校准删除并列入影响清单，需重新审核。预览包括受影响绑定、供电关联逻辑灯、场景与校准；源码程序保持原样，发布仍使用既有配置预览、CAS、停止／代次屏障和重新查询契约。schema 2 发布后，存储 CAS 拒绝降回 schema 1 或非结构化旧文档，防止丢失已保存身份。替换不发送硬件改址或 C7／C8 参数命令。

## 管理视图与命令

`GET /api/v1/modules` 仅管理员可读，最多 64 模块一页；下一页必须带同一配置版本，配置变化不能混合旧页面。返回模块 ID、物理映射、型号／验证、全部通道槽位、绑定（含隐藏供电）、实际值／质量／反馈时间与有界命令记录。当前通信摘要只计算已启用绑定输出，不能提升为全部 4／8 通道有响应；面板 LED 与电机位置无可靠状态读回时显示能力限制。接受／写出回执、恢复亮度、请求目标与实际反馈保持独立。

`POST /api/v1/module-commands` 只接受 `module_id`、`channel`、字符串 `revision`、`request_id` 和已有类型的 `command`。管理员角色与逻辑设备范围都需要通过；服务端重新解析当前模块通道并校验供电、启用状态、类型和校准，复用同一仲裁、队列及持久请求去重。隐藏供电由组合灯控制；未绑定／禁用通道不会因查询而获得控制权限。实际地址和自由十六进制报文不能作为命令目标。

底层模块查询与容量修复在独立检查点维护。模块管理的整模块／逐路查询操作接口，以及 canonical module-channel 反馈拥有者与未绑定通道存储，继续下一检查点。每个模块独立对象并不独立连接 TCP485；同一总线继续由单个 TransportOwner 负责串行读写与帧间隔。

本轮仅合成与本机回环验证；没有部署、发布现场配置、启动真实服务或更改 EA-5 的配置／控制范围。
