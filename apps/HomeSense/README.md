# HomeSense · Swift App

原生 SwiftUI 家庭中控与本地调试工具，支持 macOS 14+、iPhone / iPad（iOS / iPadOS 17+），包标识为 `com.ngboss.homesense`。家庭管理通过 HomeSense HTTPS API 与 WSS 状态推送完成；网关直连测试保留在“本地调试”。

## 主要功能与实现情况

- **日常控制**：以房间、紧凑设备卡片和场景卡片浏览家庭，点卡片进入详情控制。默认进入日常模式；管理员通过工具栏“管理／完成管理”切换配置入口，成员仅有日常入口。
- **配置管理**：楼层、房间、设备与场景编辑，支持独立空楼层、设备绑定和完整场景 JSON。编辑先保存为服务端草稿，再预览并显式发布；换绑清除旧电机校准与行程估计。模式切换保留草稿、共有页面和房间筛选，不重连、不自动保存或发布；退出管理专用页面时回到概览。
- **实时状态**：活动目录与未发布编辑稿分离，通过 WSS 合并状态更新。按目标网关的就绪状态停用离线／同步中的控制，保留同页健康目标可控；历史读回不显示为已确认，恢复不重放旧操作。选择状态、实际反馈、历史与未知分别展示；断开同步后停用控制，恢复时重新鉴权和读取状态，不重放控制或发布。
- **Keypad**：已迁移的面板父对象作为一张正方形设备卡，与灯、窗帘并列；卡内两列三行 `1 2 / 3 4 / 5 6`。按键底色和小灯点共同表示颜色并注明来源，不可操作时仍保留历史颜色。用途只执行已审核、已授权的场景或已校准窗帘映射。
- **面板子功能**：详情集中提供每键红／绿／灭灯、背光、声音与已发布工作模式的显式操作，按服务端能力和权限开放。整板设置额外要求管理员与完整六键权限；配置管理支持父对象、稳定子端点 ID、按键名称、启用和用途审核依据。
- **凭证保存**：成功鉴权后按规范化 HTTPS Origin（含端口）保存到本机系统 Keychain，不参与 iCloud 同步，启动时恢复连接。不同服务不混用令牌；普通断开、网络或 TLS 失败保留保存项，明确 401 仅清除被拒绝的对应凭证。“退出并移除本机凭证”清除本机记录，不撤销服务端令牌。
- **本地调试**：TS6-86 / TS6-86-B 地址查询与修改、参数和固件查询、手动指令、实体按键事件、按键联动 LED 及日志导出；LH-IO1600 独立地址页提供十进制软件地址读取、修改和新地址读回核对，两种协议与目标地址分别处理。

Keypad 使用[原生接口契约](../../docs/keypad-native-api.md)中的真实父对象；App 不把旧逐键条目折叠为假父对象，未迁移服务继续兼容原记录。场景选择状态已知时发明确 On／Off；未知且服务端允许相对操作时才使用相对接口，不注入物理按键或自动翻转 LED。

## 构建与运行

使用 Xcode 打开 `HomeSense.xcodeproj`，选择 `HomeSense` scheme 和 My Mac、iPhone 或 iPad。真机安装需选择自己的开发团队；也可创建已忽略的 `Signing.local.xcconfig`，填写 `DEVELOPMENT_TEAM = 你的团队ID`。首次连接允许本地网络访问。

```sh
cd apps/HomeSense
export HOMESENSE_BUILD_ROOT="$PWD/build"
bash tools/build.sh macos
open "$HOMESENSE_BUILD_ROOT/macos/Build/Products/Debug/HomeSense.app"

# iPhone / iPad 无签名编译检查；真机安装通过 Xcode 配置签名。
bash tools/build.sh ios

# Swift、内存 API 与本机模拟 TCP 回归。
swift test --scratch-path "$HOMESENSE_BUILD_ROOT"
```

每个会话固定一个绝对构建根目录，后续原地复用。`tools/build.sh` 将 Xcode 输出放在该根目录的平台子目录；Swift 测试和实际联调脚本分别显式传入 `--scratch-path "$HOMESENSE_BUILD_ROOT"`、`--scratch "$HOMESENSE_BUILD_ROOT"`，避免另建默认 `.build` 或按版本、轮次复制构建缓存。源码快照、日志和必要安装包可另存。

真机安装包使用 `tools/package-ios.py`，先签全部内嵌 Mach-O（包括根目录的 `HomeSense.debug.dylib` 和 `__preview.dylib`），再签 App，随后逐个验证签名、团队、描述文件和权限。仅执行 App 层的 `codesign --verify --deep --strict` 不能证明这两份动态库已签名。独立 App 可按下面方式打包；带扩展的 App 需要另外配置各自描述文件。

```sh
python3 tools/package-ios.py \
  --app "$HOMESENSE_BUILD_ROOT/ios/Build/Products/Debug-iphoneos/HomeSense.app" \
  --output "$HOMESENSE_BUILD_ROOT/artifacts/HomeSense.app" \
  --build-number '<构建号>' --identity '<开发签名标识>' \
  --profile '<原描述文件路径>' --entitlements '<原权限文件路径>' \
  --expected-bundle com.ngboss.homesense --expected-team '<原团队ID>' \
  --report '<私有安装包校验报告路径>'
```

安装后还应启动原测试机上的 App，核对进程和连接结果。打包工具支持 `--verify-only`，可对现有包逐一检查所有 Mach-O；它不执行安装，也不读取手机数据或钥匙串。


实际 Rust HTTPS/WSS 联调需使用独立、固定且不会被并行构建覆盖的后端二进制。将下面的占位路径替换为该文件的绝对路径，再运行：

```sh
export HOMESENSE_TEST_BINARY="/absolute/path/to/pinned/homesensed"
export HOMESENSE_KEYPAD_TEST_BINARY="$HOMESENSE_TEST_BINARY"
export HOMESENSE_KEYPAD_TEST_FIXTURES="$PWD/../../config/test/native-app-keypad"
swift test --scratch-path "$HOMESENSE_BUILD_ROOT"
```

缺少相应环境变量时，两项实际 Rust 集成测试明确跳过。测试使用隔离模拟设备、临时状态及注入的内存凭证存储，不访问用户 Keychain。

AppIcon 来自仓库的 `docs/assets/homesense-logo-v3.png`；重新生成使用 `swift tools/generate-app-icons.swift`。

## 连接与操作

在“我的家 → 服务连接”输入完整 HTTPS Origin 和访问令牌。默认管理地址为 `https://192.168.3.2:8444`，服务是否开放以部署窗口为准；它与直连网关地址不同。证书必须由系统信任并匹配服务地址。令牌不写入 URL、配置或日志，不使用 Cookie，也不跟随重定向。

鉴权成功后会保存凭证，之后可自动恢复，也可点击“使用此服务已保存的凭证连接”。“断开服务”只结束当前会话；要清除保存项，使用“退出并移除本机凭证”。配置修改需依次保存草稿、预览影响并确认发布，发布前不改变活动配置。

场景在管理编辑稿中选择房间，归属只存于 `automation.scene_metadata[].room`，不修改动作、按键规则或授权。活动场景通过 GET／WSS 的 `room:{id,name,floor}|null` 显示在对应房间；未分配、旧后端缺字段或暂时找不到房间的场景留在家庭概览，不按名称或动作推断。归属编辑需要支持此元数据字段的后端，发布前仍显示原活动归属。

房间页只显示服务端允许日常展示的灯、窗帘与明确归属的场景，每个稳定 ID 只出现一次；不按同名合并不同设备或场景，也不把程序内部的子场景推断为房间入口。面板卡片集中在“设备”列表，保留六键控制和 LED 状态；家庭概览显示灯、窗帘及全屋／未分配场景。房间内没有面板，因此已绑定面板的房间场景仍保留独立入口。专用“场景”页与配置管理保留完整授权列表，断网门禁、LED 显示与本地调试保持原样。

场景采用方形卡片，每个房间按两列三行排列，单卡最多六个场景，超出时分卡；相同 ID 不重复出现。点场景键直接走已有场景接口：瞬时场景“执行”，选择场景按明确的“开启／关闭”操作；选择未知时显示“开启”，提交明确 `on:true`，受理不会伪造选择状态。每键有详情入口，分别保留选择状态与负载反馈；断网或目标不可用时控制键置灰，历史状态仍显示。虚拟场景卡不生成实体面板或 LED 反馈。

“设备”页集中显示面板和 R818-DIN／AD8-DIN／C4-DIN 物理模块，标注十六进制总线地址；逻辑灯和窗帘位于房间与家庭概览，完整逻辑设备编辑移至“草稿与发布 → 逻辑设备配置”。管理员模块清单来自只读 `/api/v1/modules`，分页核对版本与 boot，不从逻辑灯名称推断硬件；成员不请求该清单，旧服务无接口时保留面板及既有控制，断连／降权／换版本清除模块缓存。

模块详情可显式发送服务端支持的整模块／单通道查询、开关测试、调光亮度设置和已校准窗帘的明确打开／停止／关闭；每次点击只发送一个带当前版本与独立请求 ID 的请求，无回执重放或预先修改反馈。通道动作同时服从服务端 `actions` 与逻辑绑定授权，供电继电器不绕过组合灯约束。C4 未支持的反馈查询保持不可用，面板设置沿用原详情页能力门禁；未定义的硬件改址／原始参数报文不由客户端推测。

本地调试步骤：

1. 填写网关 IP 和端口并连接，默认 `192.168.3.7:8234`；串口由网关配置为 9600、8N1。TCP 连通不等于面板响应，调试时避免其他程序向同一总线发送命令。
2. 输入两位十六进制目标地址查询。未知地址时，仅在确认隔离单面板后使用 `00` 广播查询；单字节 `00` 是有效读回。
3. 改址输入未占用的 `01–FF`，长按实体按键①并确认发送；目标为 `00` 时必须确认单面板隔离。随后查询新地址核对。App 不把写出视为改址成功，也不自动修改服务端模块配置。
4. 普通灯控、参数与固件测试需选择非零目标。手动灯控需显式切换面板模式；“按键联动 LED”先设六键绿灯，再随每次按下切换同键红／绿，释放不切换。断线、后台、切换地址或手动接管会停止联动，重连后需重新启用。

LH-IO1600 地址工具：

1. 进入“本地调试 → 网关连接”，使用透明串口网关，模块与网关的串口参数一致（出厂 9600 / 8N1），再进入“LH-IO1600 地址”。进入该页会停止 TS6 按键灯联动。
2. 输入**十进制**当前从站地址后“读取地址”。不知道地址时，先隔离总线、仅保留这一块 LH-IO1600，确认后可用厂家 `254 / FE` 广播查询；当前地址为 0 也要求隔离，读回 0 会正常显示。
3. 单模块隔离后，输入未占用的十进制新地址 `1–247`（本工具限定范围），确认发送。LH-IO1600 无需长按 TS6 面板按键；TS6 原页面的 HEX 输入和按键确认保持原行为。
4. 改址使用 `06` 写保持寄存器 `0x03EA`（PLC 41003），查询使用 `03`。报文带 Modbus CRC，经过 TCP 透明传输，不使用 Modbus TCP MBAP 头。发送前展示完整 HEX；不扫描、不重试、不自动操作继电器或更新服务端配置。
5. 写入回显仅作为诊断，点击“查询新地址核对”，用刚写出的新地址读回相同软件地址后才显示通过。即使写入超时也可手动核对；失败或断线不会伪造成功。日志包含原始 RTU 收发和结果。CRC、功能码、地址不符或多个匹配回复不会显示有效读回。

依据 [LH-IO1600 厂家手册](https://www.lonhand.com/uploads/20220909/84b22c5158a8530800d9e789f2708fe9.pdf)第 8–11 页；该型号与 LH-IO1600H 不同。手册“偏移地址”行仍带通用拨码公式，App 明确显示读取的是软件地址；用新地址重新读取核对通信是否生效。软件模拟测试不代表实物固件验证。

通信日志保留最近 600 条，可导出文本。TS6 地址查询在完整 2 秒窗口仅收到一个字节时才显示读回；异步或迟到报文不能证明设备唯一性或改址因果。协议编码和接收实现见 [TS6Protocol.swift](Sources/HomeSenseCore/TS6Protocol.swift)。

## 验证范围与限制

软件回归覆盖模式与权限、草稿保留、活动目录隔离、WSS 同步、Keychain 保存逻辑、Keypad 接口、逐目标可用状态，以及 TS6／LH-IO1600 协议与模拟 TCP；隔离的实际 Rust HTTPS/WSS 联调验证客户端与服务端契约。历史结果见 [CHANGELOG](CHANGELOG.md)。

当前面板协议没有 LED 颜色读回，最后写出不等于实测，按键事件也不能证明灯色。背光、声音与工作模式的读回格式尚未核实，实测保持未知；查询或写出成功不表示设置已确认。场景选择与负载反馈同样分别展示。

软件结果和模拟截图不能代替现场验收。真实 Keypad 的迁移、权限和硬件效果，以及手机 Keychain 首次保存／重开恢复、前后台同步，需在相应服务窗口核对。进入后台或“本地调试”会暂停家庭状态流，返回后重新同步；本地按键联动仅在前台且网关连接有效时工作。

## 点击与请求诊断

六键卡的实际 Button action 同步产生 `button.click` 和随机 `operation_id`，随后异步处理沿用同一 ID，依次记录门禁、开关方向、HTTP 发送与结果，以及 `operation.complete` 和单调时钟耗时。每个控制请求仍单独生成原有的 `request_id`；`http.send` 和 `http.response`／`http.failed` 同时包含它和操作 ID，可与服务端回执对应。HTTPS 共用层也记录只读请求，区分 HTTP 非成功状态、响应格式错误、超时、取消及传输失败，不增加刷新、重连或重发。

禁用 Button 不会收到系统派发的 action，因而不能声称捕获了禁用表面的触摸。卡片出现和门禁状态变化时记录去重的 `gate.state`，包括连接、忙碌、当前快照、推送、映射、目标可用、授权、场景选择及相对操作许可；此类状态记录不冒充点击，没有 `operation_id`。getter 本身不写日志，原忙碌和安全门禁保持。

系统日志为 subsystem `com.ngboss.homesense`、category `ControlTrace`。App 内“服务连接 → 导出诊断日志”可保存文本；导出仅包含当前和经格式校验的上一会话。磁盘中固定保存：

- `Library/Application Support/HomeSenseDiagnostics/control-trace.jsonl`：当前会话。
- `Library/Application Support/HomeSenseDiagnostics/control-trace.previous.jsonl`：新会话首次写入时移位保留的上一会话。
- 同目录 `control-trace.pending`：异步原子替换时唯一临时文件。

每会话默认最多 512 条，单文件硬上限 256 KiB；当前与上一会话最多 512 KiB，含临时快照磁盘最多 768 KiB。异步队列仅保留最新一份待写快照；旧条目从环形记录淘汰。目录权限 0700、文件 0600，目录排除备份，iOS 文件使用首次解锁后的数据保护。写入失败不改变控制结果；内存记录仍可导出。未完成异步写入即终止进程时，末尾少量事件可能尚未落盘；日志不是服务端执行回执或实体反馈证明。

仅记录固定字段：操作／请求 UUID、会话内加盐匿名目标标识、按键号、布尔状态、方法、固定路由／配置路径模板、HTTP 状态、字节数、白名单错误和 outcome，以及白名单 NSError domain／数字 code。UTC `time` 可换算到本地时区，`sequence` 保序，`elapsed_ms` 使用单调时钟。设备／场景名称和原 ID、服务地址、令牌、Authorization、请求／响应全文、错误 description／userInfo 均不写入。未知后端错误码记为 `other`，已列入当前后端固定拒绝码白名单的原值可以检索。开启日志不代表六键再次关闭故障已修复。

已连接并解锁的测试 iPhone 可只取上述已知文件，无需读取 Keychain 或复制整个容器。由取证操作者替换设备标识和本机保存位置：

```sh
xcrun devicectl device copy from --device '<原测试机标识>' \
  --domain-type appDataContainer --domain-identifier com.ngboss.homesense \
  --source 'Library/Application Support/HomeSenseDiagnostics/control-trace.jsonl' \
  --destination '<本机私有目录>/control-trace.jsonl' --timeout 20
```

上一会话同样只取 `control-trace.previous.jsonl`。文件不存在意味着该会话尚未成功落盘，不能据此认定没有点击。macOS 定向系统日志可使用：

```sh
log stream --style compact --predicate 'subsystem == "com.ngboss.homesense" AND category == "ControlTrace"'
```

手机也可在 Mac“控制台”中选择该已连接设备，再按上述 subsystem／category 筛选；不收集全机 sysdiagnose。定位时从 `button.click` 的操作 ID 串联门禁、方向和 HTTP 终态，再用 `request_id` 对照服务端。日志取回不触发设备控制，不要求使用者复述日志内容。

## 六键卡片窗帘方向键

后端为经过审核的窗帘方向键提供 `card_action`（`motor_press` / `repeat_to_stop`）时，原生六键卡片发送单次 `keypad_press` 意图，包含当前配置版本字符串和每次点击的新请求 ID。是否把同方向再次点击改为 STOP，由服务端 Admission 根据已成功写出的运动轨迹原子判定；App 不读取 LED 或运动估计来推断，不先读后发第二条控制，也不因超时或未知结果自动重发。

该模式由服务端配置和 DTO 决定，可用于书房、画室及后续房间，客户端没有房间、面板或设备 ID 特例。`card_action` 必须与原明确电机映射的目标、父对象、按键号一致，路径和命令形状也须符合契约；非法非空元数据禁用卡片动作，不静默回退为开／关。空值或缺失兼容旧服务端原有动作。

新 `motor_press` 意图同时要求父面板与目标电机所在网关可用；父对象的 `can_control` 不作为子键授权条件。详情及旧绝对卡片动作仍沿用原目标门禁。

详情页沿用原 `action` 中的明确 Open／Close／STOP；独立 STOP 键、场景键、LED 设置、Apple 家庭和实体面板均不因卡片模式改变。卡片和详情分别记录门禁状态，继续使用既有点击／请求／响应关联日志、来源权限、校准、逐目标可用性和忙碌门禁。LED 仅展示服务端推送的实测或最后写出，不增加本地计时与预先变色。软件协议测试通过不等于新后端语义或实体窗帘已验收。
