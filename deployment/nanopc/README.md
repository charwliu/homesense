# nanopc 测试部署

## A5 大型导入报告的本机存档

`20261011-a22-import-bundle` 的 `coverage-report.json` 和 `study-blockers.json` 保存在 `deployment/nanopc/private/evidence/20261011-a22-import-bundle/`，不再纳入 Git。公开的审计摘要及 SHA-256 保留在 `docs/evidence/stages-a-c-audit-20261011.json`；其他较小的导出文件仍在 `deployment/nanopc/evidence/20261011-a22-import-bundle/`。新克隆的仓库不包含这两份原始报告。

## 独立候选版本：20261011-a21

a21 已独立安装并通过 ARM64 包内哈希、模拟／Live 回环各两次重启及面板 LED 回环验证。工作区 226 项通过。包含延迟任务代际／故障取消、Admission 配置发布屏障与读写版本隔离；服务在线热重建尚未公开。current 保持 a19，未启动现场控制或自启动，原身份目录未用于此次隔离测试。详情见 `20261011-a21.json` 和 `evidence/20261011-a21/remote-smoke.log`。

## 独立测试版本：20261011-a20

a20 已独立安装，current 保持 a19。书房面板 41 第 2 键绿色保持约 60 秒后恢复红色，用户确认两次颜色变化，恢复命令成功退出。未发送第 6 键或按键事件。213 项工作区回归、ARM64 构建和远端回环验证通过。记录见 `20261011-a20.json` 及 `evidence/20261011-a20/panel-key2-field-result.json`。

## 当前版本：20261011-a19

current 已切至 a19，沿用 revision 2、原身份和配对，HAP c#=3。210 项工作区回归、ARM64 构建及远端两次模拟／两次 Live 回环通过。用户确认 a18 已无亮度回跳，但单独开灯仍恢复 100%；a19 将关灯状态与保留亮度分开：On=false，Brightness 保持已确认的非零记忆值，且关灯事件重新发送该亮度。单独 On=true 恢复记忆，显式 Brightness=100 仍照常执行。未知／断线时两个特征仍报告不可用，不借记忆伪造在线状态。字段级诊断只记录 On／Brightness 和设备标识，用于区分客户端明确亮度与恢复请求。

a18 的中间调光事件抑制和持久亮度保护继续保留；没有有效记录才用配置默认值。停止状态备份：`backups/stopped-pre-a19-20261011-082727`。切换时实际及记忆均为上一轮操作留下的 100%；随后用户复测确认 50% 关灯后单开恢复正确且无回跳。本轮另记录两次调光后查询超时并自动重连，稳定性尚未验收。未启用自启动，20 分钟限时桥的状态须查现场进程。

## a15 组合灯部署说明

a15 部署当时 current 指向 releases/20261011-a15；配置 revision 2，仅书房主灯 03/4 供电与 12/4 调光。原 tests/live-study-lan-a11 身份及配对保留，HAP c#=3。已完成双路首次同步和原控制器认证，20 分钟限时现场试验已启动；实时状态须查进程，不依靠此文档判断。未开启自启动。a14 全回归 201 项通过，a15 增加离线发布测试通过；a15 ARM64 校验、两次模拟与两次 Live 回环通过。真实调光和恢复亮度等待用户观察，不算验收通过。

当前启动命令（先确认没有同一实例）：

```sh
cd /users/alarm/apps/homesense
current/homesensed serve-live current/config/study-main-light.live.json tests/live-study-lan-a11 192.168.4.1:51827 1200 --coexistence-confirmed
```

配置变更须停止服务后通过 `publish-config FILE STATE_DIR EXPECTED_REVISION` 发布，不能直接改 active_configuration。新版本使用 runtime.sqlite schema 4，旧 a13 不支持。停止状态备份在 `backups/stopped-pre-a15-20261011-080026`；回退前先备份当前库，再恢复匹配旧版本的运行库。身份库必须保留最新配对／撤销状态，不可盲目覆盖为旧备份。EA-5 配置未修改。

跨 VLAN 仍依赖 Mac 临时 DNS-SD 记录；结束试验后清理对应发布进程。当前记录需 sf=0、c#=3，与旧未配对记录不同。后续持久发现尚未部署。

## 历史版本：20261011-a13

新增逐通道电机方向／STOP 校准记录，逻辑窗帘及手动 LED 命令入口复用持久化 Admission 和唯一传输所有者。191 项工作区回归、ARM64 构建、SHA256 及两次模拟／两次 Live 回环通过。current 已切至 a13，旧版本保留；没有运行服务或自启动。现场配置仍只启用书房 03/4；窗帘和面板未启用。a13 的实物写入验收未完成。

## 历史版本：20261011-a12

只读现场补验通过：限定 03/4 查询的代理注入一次 EOF，服务自动建立新连接并重新读取真实反馈，epoch 2→3，0 控制命令。实际网关掉电及物理控制仍未验收。日志与报文见 `evidence/20261011-a12/`。

已安装到独立 release 目录，SHA256 校验、185 项工作区回归、两次模拟与两次 Live 回环启动通过。包含逐设备周期查询调度、连接恢复诊断及通知背压隔离回归。尚未切换 current，尚无此版本现场控制证据；a11 限时配对试验已结束，0 配对、0 控制命令，临时 Bonjour 已清理，身份保留。

## 历史版本：20261011-a11

现场验收（已结束，0 配对／0 命令，临时发现已清理）：曾启动 20 分钟限时单设备桥 `HomeSense`，端点 `192.168.4.1:51827`，身份目录 `tests/live-study-lan-a11`，仅启用书房 03/4 主灯。VLAN3 发现由 Mac 临时发布；这不是常驻部署。启动于远端时间 2026-10-10 17:44:08 UTC，运行是否仍有效须检查进程。实际 HomeKit 配对及控制验收尚待日志确认，未启用自启动。

修复相同值反馈误判超时：查询完成按有效反馈计数判断，不使用去重后的状态版本。11 项相关回归、两次 nanopc Live 回环通过。现场 03/4 只读复测 10 秒、4 次查询均返回关，单 TCP 连接、无控制帧、无命令记录，正常退出。没有常驻服务或自启动。

## a10 历史记录

Live 服务、重连监督与周期查询已部署；184 项工作区回归通过，nanopc 上两次模拟和两次 Live 回环隔离冒烟通过。当前没有运行中的测试服务或自启动。书房 03/4 单通道 Live 配置已按已有开启／查询／恢复关闭证据生成并校验，尚未启动现场服务。EA-5 未修改。

## a9 历史记录

保留控制器配对标识的原始大小写，补充配对增删计数日志。51 项 HAP 单元测试、13 项 SQLite 测试、3 项加密 HTTP 回归及两次 ARM64 隔离冒烟通过。a8 现场中枢同步后配对数量变成 0、返回 401；大小写是否触发删除仍待重新配对确认。备份 identity-before-a9.sqlite 后启动 20 分钟模拟服务，不恢复已撤销密钥，无自启动。

## a8 历史记录

新增 TCP 来源和会话结束原因日志；5 项相关回归、ARM64 构建、校验和及两次隔离冒烟通过。沿用 tests/home-lan-a4 身份限时运行，无自启动。家庭曾恢复控制后再次未响应，稳定性尚未验收。

## a7 历史记录

诊断版已通过隔离冒烟，current 指向 a7，使用原 tests/home-lan-a4 配对身份临时运行。用户已添加但报告未响应；协议状态日志用于定位，不记录请求体或密钥。没有启用自启动，尚未确认控制恢复。

## a6 历史记录

最新稳定直接依赖迁移版已部署，SHA 校验及两次模拟冒烟通过，身份稳定且退出释放监听。current 指向 a6，保留旧版本；无自启动。构建指纹见 20261011-a6.json。跨 VLAN Bonjour 和 iPhone 实机配对仍待验证。

## a5 历史记录

current 指向 releases/20261011-a5。包内校验、两次模拟冒烟和身份保持通过，Mac 未配对 HTTP 返回 401；60 秒模拟服务已退出。更换 mDNS 后跨 VLAN Bonjour 仍未发现，不能认定发现问题已解决。详情见 20261011-a5.json。此版本早于后续“全部最新稳定库”迁移。

## a4 历史记录

current 指向 releases/20261011-a4，保留 a1/a2/a3。模拟冒烟通过；LAN 192.168.4.1:51826 从 Mac 返回未配对 401，但 Bonjour 未发现。libmdns 0.6.3 使用默认组播接口并枚举全部非回环地址，没有按 HAP host 限制。诊断进程已结束、身份目录 tests/home-lan-a4 保留；待修正接口选择并继续 iPhone 配对验证，无自启动。构建索引见 20261011-a4.json。

## a3 历史记录

current 已切换至 releases/20261010-a3，保留 a1/a2。包含只读查询、反馈过期、HAP 离线通知及 EVENT CRLF 修复；Rust 1.99 ARM64 构建，校验和、四路禁用配置校验与两次隔离模拟启动测试通过。部署时从导入报告提取 configuration 对象作为运行配置，不能直接把整个导入报告交给 check-config。未启动常驻服务或自启动。

现场只读查询结果（地址十六进制）：03/4 开，07/6 开，11/5 100%，12/4 100%；反馈时间 27–30ms。原始 JSONL 见 evidence/20261010-a3，构建和结果索引见 20261010-a3.json。只证明这四路查询和格式匹配；未验证关状态、其他亮度、控制效果、重连或实际 Apple 家庭配对。所有设备继续 disabled/Pending。

## a2 历史记录

2026-10-10 已部署并测试 `20261010-a2`。SSH 使用 `nanopc.lab.vixal.xyz`（192.168.4.1）；本轮 Mac 无法解析 `nanopc.local`。安装位置为 `/users/alarm/apps/homesense`，`current` 指向 `releases/20261010-a2`。保留 a1 测试版本。未安装 systemd 单元或启用自启动，测试结束后无 homesensed 进程。

二进制来自当前未提交工作区，基线提交和源码／二进制 SHA256 见 `20261010-a2.json`。使用 Rust 1.99.0，目标 `aarch64-unknown-linux-gnu`；本机 Zig 0.17 无法处理 Rust 默认 `--fix-cortex-a53-843419`，最终以 LLVM 23.1.2 和从 nanopc 只读取得的 glibc 2.41 sysroot 链接，保留该参数。未修改 nanopc 已安装的 Rust 1.95。

## 已执行验证

- 包内 SHA256 全部核对通过；ARM64 二进制执行成功。
- `demo 30` 与书房面板配置校验通过。
- 本机模拟 HAP 两次启动，未配对读取返回 401，持久化身份、AID、配置编号不变；退出释放监听端口。
- a1 测试脚本在 Linux TIME_WAIT 上误判端口仍占用；a2 用 SO_REUSEADDR 重新绑定并 listen 验证，没有改变二进制。
- 从 nanopc 到 192.168.0.10:50007 的被动采集成功，运行 10085ms，0 帧、0 噪声／残帧，正常到时结束。只证明连接和接收循环可用，不能证明查询／反馈或控制已经验收。

## 复测

```sh
ssh nanopc.lab.vixal.xyz
cd /users/alarm/apps/homesense/current
python3 smoke-test.py "$PWD" /users/alarm/apps/homesense/tests
./homesensed observe config/study-panel.draft.json hopebeauty 10 --coexistence-confirmed
```

用户已确认网关 50007 支持与 EA-5 同时连接。被动采集不发送应用报文；书房面板六键仍为 pending/disabled。EA-5 控制范围及网关、路由和防火墙配置均未修改。

模拟服务只监听回环、不发布 mDNS；本次部署还不能直接用 iPhone 家庭 App 添加，也没有启动真实设备控制服务。现场 B/C 查询、反馈及控制验收继续保留为未完成。
