# RS485 模块注册表与稳定引用

引用层检查点实现稳定 ID、配置草稿和管理入口。新的 `ModuleTruth` 在 Feedback 内唯一拥有可变模块通道观测，按注册表建立全部通道，不依赖逻辑设备是否绑定或启用；逻辑设备、组合灯和管理页均读取同一物理真值的投影。[原未绑定通道反例](evidence/stage-e-browser/module-channel-truth-gap-root-check.json)保留为历史，新的合成回归直接验证相同通道的反馈被保存且不新增设备或控制权限。

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

底层模块查询与容量修复在独立检查点维护。管理员主动查询接口见下节；启动／重连／发布／周期查询现在覆盖全部已登记可读输出通道，包括未绑定、禁用及无逻辑别名模块；就绪要求本次连接每路都有新鲜反馈。容量按物理查询帧数预检，PRO／C8 逐路、白名单型号整模块查询。无读回能力的面板 LED／电机位置仍未知；读回不创建逻辑设备或控制授权。每个模块独立对象并不独立连接 TCP485；同一总线继续由单个 TransportOwner 负责串行读写与帧间隔。

本轮仅合成与本机回环验证；没有部署、发布现场配置、启动真实服务或更改 EA-5 的配置／控制范围。

## 模块通道真值与逻辑投影

`LogicalDevice` 保存模块 ID／通道和业务属性，不保存网关／物理地址。`ResolvedBinding` 是当前不可变配置版本的传输适配对象，旧 `Device` 类型名仅保留为其兼容别名；schema 1 的物理字段也只在此适配层解析。已有传输调用接口保持兼容，不新增总线连接。

每个模块的全部通道在 `ModuleTruth` 注册，内部通用状态容器使用私有物理端口键；唯一可变观测不以业务 DeviceId 保存。逻辑设备 ID 只是启用通道的只读别名，组合灯由调光及供电两份物理样本派生。名称／逻辑绑定投影变化不重写原物理样本；模块换址或配置发布必须建立新拥有者与新代次，不能把旧地址反馈解释成新模块状态。通道观测当前保存在运行期内存，重启不把历史观测恢复成新鲜反馈。

未绑定及禁用通道的合法反馈保留实际值、质量和时间，管理槽位可见；它们不加入逻辑设备列表、规则反馈或可控授权。非法原始值使质量变为 unknown，保留最后有效样本用于诊断，并从规则及原生场景资格中移除；新的合法反馈才恢复 confirmed。接受／写出／设定／恢复亮度仍在原命令存储中，与观测独立。面板按键值不能解释成 LED 状态，窗帘位置也不推断。

逻辑事件是物理真值派生的只读日志；未绑定反馈不制造逻辑事件缺口。批量过期超过物理事件环容量时，逻辑订阅显式要求重新取 snapshot，再从新 cursor 继续，不静默丢失质量变化。管理通信摘要仍只计算启用绑定，不能称全部通道在线。

逻辑投影版本与 canonical 物理版本分开：逻辑 snapshot 的 state_version 等于自己的 cursor.sequence，逐别名状态／事件版本对应最后的逻辑事件。版本元数据没有复制或拥有实际观测；canonical 通道保留原物理版本。HAP 的严格一致性校验保持原样，超出物理事件环时只通过明确的 snapshot resync 恢复。

## 管理员主动查询

`POST /api/v1/module-queries` 请求为 `{ "module_id": "module-dimmer", "scope": { "kind": "channel", "channel": 3 }, "revision": "2", "request_id": "query-once" }`；整模块改用 `scope: { "kind": "whole_module" }`。只允许管理员管理读取，不接受 gateway／address／hex 或其他自由字段。ID、版本、Origin／会话鉴权、当前运行期 boot／transport／期限、型号能力和执行上下文均需通过检查，写出前再次从当前注册表解析。

白名单 AD8／GT-E4／GT-D8／GT-L8 使用 A0 F2；PRO 和 GT-C8 仅按协议已有能力逐路展开，不解释歧义项为聚合查询。无输出读回能力的面板／电机、未审核能力、越界通道及旧配置被拒绝。合法未绑定／禁用／隐藏供电通道可读，原控制授权和逻辑设备集合不变。

读取使用现有 Arbiter 有界队列、持久 command_journal、PreparedFrame 和原 TransportOwner 写出。内部 read-ticket 标识不是逻辑设备，不加入设备图或授权；同一总线无新连接，沿用优先级公平性和写间隔。容量或权限拒绝不留下部分批次，过期、撤权、配置发布取消及短写／断线沿用现有审计；重启不恢复待写查询。

返回的是请求已接受，command ID 使用字符串。`GET /api/v1/module-queries/{request_id}` 只读该管理主体、当前 boot／版本／代次的持久写出回执及当前通道反馈；没有自动重发。已写出不等于收到应答，当前新鲜反馈也不证明来自此请求。状态明确 `feedback_causality: "not_established"` 和 `atomic_snapshot: false`，不宣称多个通道来自同一原子采样。

管理页整模块／逐通道查询按钮包括无逻辑绑定的槽位，纯 GET inventory 不发任何报文。显示启用绑定回复、全部通道当前新鲜／已有样本覆盖、最近实际反馈时间，分别展示接受／写出／实际反馈，不以 TCP 在线代替模块响应。
