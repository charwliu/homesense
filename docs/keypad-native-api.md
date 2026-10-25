# Keypad 原生接口

本契约定义管理 API 的面板父对象、逐键状态和设置能力。父对象属于配置 `keypads`，不是把现有逐键行在客户端分组。物理观测仍由模块所有者提供。一个 TS6-86 对应一个稳定父 ID、`module_id`、名称和房间；旧 LED DeviceId 被移入父对象的 `keys[].led_endpoint`，继续供规则、权限、审计和 HAP 解析。父 ID 不授权整个面板。

`GET /api/v1/devices` 的 `devices` 与 `/api/v1/events` 的 `devices` 共用以下记录；已迁移父对象的旧 LED 行不再作为顶层记录出现。未迁移配置保持原格式。增量推送中父对象始终携带完整六键快照，不能将未出现的键复位或推测为关闭。

```json
{
  "id": "panel:example", "kind": "Keypad", "name": "示例面板",
  "module_id": "module:example", "room": {"id":"room","name":"示例房间","floor":"1层"},
  "quality": "Unknown", "can_control": false,
  "capabilities": {"key_events":true,"led_colors":["off","red","green"],"led_readback":false,"backlight_write":false,"work_mode_write":false,"sound_write":false},
  "keys": [
    {"number":1,"label":"1","configured":true,"endpoint_id":"legacy:key:1","can_write_led":true,
     "key_event":{"pressed":null,"quality":"Unknown","observed_at_ms":null,"source":"button_feedback"},
     "led":{"observed":null,"quality":"Unknown","source":"unknown","last_request":null,"last_written":null},
     "action":null},
    {"number":2,"label":"2","configured":false,"endpoint_id":null,"can_write_led":false,
     "key_event":{"pressed":null,"quality":"Unknown","observed_at_ms":null,"source":"button_feedback"},
     "led":{"observed":null,"quality":"Unknown","source":"unknown","last_request":null,"last_written":null},
     "action":null}
  ],
  "settings": {"work_mode":{"configured":"manual","observed":null,"can_write":false},"backlight":{"observed":null,"can_write":false},"sound":{"observed":null,"can_write":false}}
}
```

实际数组固定按行优先为 1、2、3、4、5、6。示例省略后四键，仅为缩短文档。未授权键等同未配置：隐藏其 endpoint、名称、报文、写出历史和动作。只有获授权且已启用的旧子端点可见；没有可见端点则不发布该父对象。`can_control` 对父对象恒为 false，客户端不能发整面板电源命令。

LED 实测值目前始终未知：现有反馈协议只有按下／释放，没有 LED 颜色读回。`last_request` 为本次启动、当前配置的审计意图，字段 `command/status/color/transport_epoch/control_epoch/updated_ms/current_transport_epoch`；`last_written` 仅来自 `written` 状态，颜色红／绿，source 为 `last_written`，仍不表示实际 LED。写出历史每批最多读取64条；若窗口中无 written 记录，source 为 unknown，不从按键或意图推断颜色。current_transport_epoch 只比较传输 epoch，不表示在线；断连时 Written 仍是历史，重启不从其他 boot 恢复成当前状态。不能将 pressed=true 显示为红灯，不能把 accepted/writing/uncertain 显示为写出成功。LED 灭使用已验证的 F0 编码，与红／绿的布尔接口分开。

显式 LED 设置使用已有 `POST /api/v1/device-commands`：`{"device":"legacy:key:1","request_id":"UUID","command":{"action":"panel_led","on":true}}`。true 红、false 绿，需该子端点的控制权限、已核实 TS6 型号和已配置 manual LED 模式，继续走既有审计、准入、队列和唯一传输所有者。灭灯用同一子端点与 `{"action":"panel_led_off"}`。客户端不能绕过统一按键执行入口自行翻转 LED；来源提示见[统一按键入口](keypad-actions.md)。

若配置明确设置经过审核的 `scene_toggle` 动作，`keys[].action` 返回 `{"kind":"scene_toggle","scene":"scene-id","path":"/api/v1/scene-relative-actions"}`。仅同时有该键控制范围及场景控制权限时返回；客户端点按时先读取当前选择状态：已知开启／关闭分别调用明确的关闭／开启接口，只有选择状态未知且服务端允许相对操作时才调用 relative 接口；无法判定或不允许时禁用动作。继续使用原有 request_id/未知结果不重试规则。这是 HomeSense 手动场景入口，不伪造物理按键事件，也不启用 EA-5 或禁用规则。另支持经过明确审核并绑定已启用、已校准窗帘目标的 `motor` 动作：`{"kind":"motor","device":"motor-id","command":{"action":"open"},"path":"/api/v1/device-commands"}`，action 是 open／stop／close，需同时有该键与目标电机控制权限。未配置时 action=null，按键不发送动作。

背光、声音与工作模式集中在 settings。`can_write` 仅对管理员且完整显式覆盖该面板六个已启用子端点时为 true；单键权限不扩大。部分配置/部分授权维持 false，补齐六键和授权需显式审核。每项都有 observed=null、quality=Unknown、source、last_request、last_written；与LED历史相同，Written 不等于实测。configured 工作模式仅为配置声明。参数查询 C1/C5/C7 的返回格式仍未经核实，不解释成实际参数。

显式参数命令用 `POST /api/v1/keypad-settings`，必填 keypad、revision（十进制字符串）、request_id、command。例如 `{"keypad":"panel:example","revision":"10","request_id":"UUID","command":{"action":"backlight","mode":"dim_after_ten_seconds"}}`。命令包括：

| action | 参数 | 编码 |
|---|---|---|
| read_backlight | 无 | C5 00 |
| backlight | mode: dim_after_ten_seconds／bright／dim | C6 01／02／03 |
| read_sound | 无 | C1 00 |
| sound | enabled: true／false | C2 01／02 |
| read_work_mode | 无 | C7 00 |
| work_mode | mode: manual／self_reset | C8 02／01 |

经过既有准入、审计、队列与唯一传输所有者写出；借用该父对象1键的内部端点作为传输/审计锚点，父设置权限在 API 和运行时重复核对，不复用单键 LED 的控制入口。工作模式值必须与当前已发布模块的 led_mode 一致：改成 self_reset 需先审核配置并处理不兼容的 LED 联动规则，然后发布配置，再显式发硬件命令。不能因配置发布而自动发送。打开卡片、迁移、切换 App 模式和连接时均不发送 C8 或其他参数命令。

界面使用正方形主卡、两列三行 1–2／3–4／5–6，详情放卡外展开区域。日常/管理 App 模式与硬件工作模式无关。客户端只展示服务端公开的能力，不能扩大执行范围。

## 源凭证派发租约

新整板参数与单键 LED 原生 API 均携带精确凭证 generation；Cookie 来源额外绑定该会话及其有效期。相同 controller 的其他有效凭证不能替代这一来源。运行时重核角色／六键范围，准入和派发检查租约，传输 owner 在实际同步写前再次检查，并在有界写期间持有凭证／会话读锁。已完成降权、缩权、撤销或登出后，旧租约不能产生参数或 LED 帧；写锁的线性化意味着已经开始的有界写可以先于撤销完成，不承诺中止已发送字节。


普通灯光、窗帘、场景和发布同样使用各自的精确来源授权，详见[授权契约](source-authorization.md)。受信任本地接口与 HAP 按独立来源规则处理，不能借用管理凭证证明。
