# 同一场景的执行入口与选中状态

某些模式的灯光组合和亮度完全相同，但选择状态与按键 LED 不同。这时不能仅凭灯光反馈猜出最后选择的模式，也不应在房间中同时显示一个执行程序和一个灯光配方。

可显式声明 `scene_presentations`：

```json
{
  "scene_presentations": [
    {
      "scene": "reading-action",
      "selection_scene": "reading-profile",
      "review_evidence": "已审核的执行入口及其原生选中状态"
    }
  ]
}
```

`reading-action` 必须是已启用的即时程序，`activate` 明确开启目标，`deactivate` 明确关闭同一范围；两条分支均为平铺的灯光／LED 命令和本组选择变量的整数赋值。`reading-profile` 必须是原生场景组中的已启用成员。开启向量须与成员一致，关闭须覆盖同一组灯并全部关闭，连同 LED 在内的两分支输出端点集合也必须相同。

两个结果都必须有 `component_mode_confirmations`：对应同一 `shared_selection_group`，在命令实际写出及真实反馈符合后确认本组变量；仅受理、写出或旧数值不能建立新的已确认选择。关联的两个定义保持同一房间。不能串联、重复使用一个内部配方，或让六键仍直接绑定内部配方。

公开目录只返回 `reading-action`，并附上 `reading-profile` 的选中状态和原有证据。内部配方继续用于原生组的目标计算，但不再是第二个公开动作；即使凭证包含其旧 ID，公开 HTTP 场景接口也不能直接执行它。不根据显示名称合并或隐藏场景。

- 房间／HTTP 的明确开启始终执行 `activate`，明确关闭始终执行 `deactivate`，模式未知时也不将关闭改为开启。
- 绑定该动作的六键 `scene_execute` 采用同一变量的原子切换：本模式已选中则关闭，其他模式或未知则明确开启。不同模式即使灯光配方相同，也不会被误当成同一模式关闭。
- 同组多个关联动作，或关联动作与其内部配方混在一个批次中，在输出前拒绝，避免靠 AST 顺序决定最终模式。
- 未配置本项的旧即时程序、普通场景及相对控制行为保持原样。关联动作不提供“未知时原生 Toggle”入口。

App 仍按服务端返回的场景 ID 控制和显示状态；房间编辑会依据显式关联同时维护配方的归属，保留动作内容、其他字段及无关联的同名场景。

原生集成夹具位于 `config/test/native-app-scene-presentation/`，只含合成地址与回环网关。准备当前源码的普通Mac CLI后执行（输出目录必须不存在，构建共用 `target/swift`）：

```sh
python3 apps/HomeSense/tools/run-scene-presentation-integration.py \
  --binary /absolute/path/to/homesensed \
  --sha256 SHA256_OF_THAT_BINARY \
  --output /absolute/path/to/new-evidence-directory \
  --scratch "$PWD/target/swift"
```

该工具生成独立HTTPS证书和令牌，仅启动本机CLI与模拟总线，不访问家庭设备或手机；检查实际Swift测试未跳过、七条灯光写入与预期一致、未改动其他通道，并保存请求／响应、状态推送结果和报文证据。

## 场景详情的灯光成员

`GET /api/v1/scenes` 为每个可见场景返回 `lights: [{id, name}]` 与 `lights_complete`。成员来自该场景命令的两个条件分支、嵌套场景和明确关闭配方；仅用于条件读取的设备、面板 LED 和窗帘不计入。组合供电按逻辑灯光展示，同一 ID 只出现一次。每个成员另行服从设备查看授权，场景权限不会扩大设备权限；列表不完整时不返回被隐藏成员的 ID 或名称。

管理场景详情使用已有设备推送中的 `observed` 展示实测开关和亮度，目标覆盖层 `reported` 不作为实测值。断连或反馈过期时标为历史／不可用，待确认同行显示。进入详情不会执行场景或新增硬件查询；旧服务缺少成员字段时明确显示清单尚不可用，不按房间或名称推断。
