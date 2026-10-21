# ngr 单实例双 VLAN 模拟部署

验证日期：2026-10-12。仅模拟运行，不接管真实设备。

## 当前拓扑

```text
VLAN3 控制器 ────────────────► ngr 192.168.3.2:51828
                                       │
VLAN4 控制器 ─► 现有核心路由 ──────────┘
                                       │
                           一个 HomeSense 模拟实例
                           一份身份、一盏模拟调光灯

mDNS 发现：ngr 的 br1 / br2 之间仅中继该模拟桥的服务与主机记录
```

VLAN4 发现后连接同一个 `192.168.3.2:51828`，不需要第二个 HomeSense 进程，也不监听 `192.168.4.2:51828`。VLAN4 至 VLAN3 的 TCP 可达性依赖现有核心路由和 ACL；未改核心配置，未开启 ngr 的 IP 转发。当前验证使用 IPv4。

| 组件 | 配置与状态 |
| --- | --- |
| HomeSense | `homesense-simulation@3.service`，active / enabled |
| 发现中继 | `homesense-simulation-mdns.service`，active / enabled |
| 原 VLAN4 模拟实例 | `homesense-simulation@4.service`，inactive / disabled，状态目录保留 |
| 运行目录 | `/home/build/apps/homesense/simulation` |
| 唯一活动状态目录 | `simulation/vlan3` |
| HomeKit 名称 | `HomeSense Simulation` |
| 模拟桥标识 | `DE:78:BF:C4:78:F8`，沿用此前 VLAN3 模拟身份 |
| mDNS 主机名 | `homesense-de78bfc478f8.local` |
| 配置 | 自带 `config/simulation.json`，仅一盏模拟调光灯 |

使用已构建提交 `2a37c1d34d5c`，没有打包工作区的未提交修改。二进制 SHA-256：`c8be2fae575c3febbfee72dae7aa5f0a0232bc463a112783736dfdd05085fa68`。

nanopc 的生产配对、配置与服务未改动，未复用生产身份，也未建立真实设备连接。

## 发现中继

独立 Avahi 进程读取 `/home/build/apps/homesense/simulation/avahi-simulation.conf`；没有修改系统默认 `/etc/avahi/avahi-daemon.conf`，系统原有 avahi-daemon 服务保持未启动。

```ini
[server]
host-name=ngr-simulation-relay
use-ipv4=yes
use-ipv6=no
allow-interfaces=br1,br2
enable-dbus=no
[publish]
disable-publishing=yes
[reflector]
enable-reflector=yes
reflect-ipv=no
reflect-filters=HomeSense Simulation._hap._tcp.local,homesense-de78bfc478f8.local
```

过滤条件限定模拟桥服务名及其主机名，不使用 `_hap._tcp.local` 全类别转发。未来若重建模拟身份或修改名称，需要同步更新过滤条件。原生 HAP 服务仍负责发布动态 TXT 信息，中继随之转发，不另行维护静态配对状态。

## 实测结果

从 nanopi5（192.168.3.1）和 nanopc（192.168.4.1）分别进行检查：

- PTR、SRV、TXT、A 全部解析成功，均指向上述相同身份、地址和端口。
- 未配对 `/accessories` 请求返回 401。
- 临时模拟控制器完成 Pair Verify 和加密 HTTP 控制：VLAN3 写 30%，VLAN4 读到 30%；VLAN4 写 60%，VLAN3 读到 60%。均等待模拟设备反馈后确认读值，访问的是同一份运行状态。
- 测试控制器运行在 ngr，通过两个路由器上的短时 TCP 通道建立连接，HAP 连接分别从对应路由器 LAN 地址发起。不是 Apple Home 客户端测试，也不覆盖其首次配对流程。
- 测试完毕删除临时控制器配对及其私钥文件；模拟桥当前无配对控制器。
- 重启 HomeSense 和中继后，桥身份不变，VLAN4 完整发现再次通过；已核实 ngr 只有一个 `homesensed` 进程。
- 原有 3 项模拟 HAP / 反馈测试通过，日志在 ngr `simulation/validation.log`。

验证工具保存在本目录：`simulation-discovery-probe.py`、`simulation-path-relay.py`、`simulation-network-probe.rs`。Rust 工具编译位置为已构建源码的 `crates/homesensed/examples/`，复用相邻测试支持代码；仅允许用于隔离的模拟状态目录。临时 TCP 通道仅接受 ngr 地址，最多运行 180 秒，不设为常驻服务。

## 启动与停止

HomeSense 服务以 build 用户运行，仅状态目录可写。程序单次运行上限 24 小时，systemd 退出后等待 5 秒重新启动，每天有短暂中断。已验证服务重启与 enabled 状态，未重启整机。

停止模拟并取消开机启动：

```sh
sudo systemctl disable --now homesense-simulation@3 homesense-simulation-mdns
```

恢复运行：

```sh
sudo systemctl enable --now homesense-simulation@3 homesense-simulation-mdns
```

上述操作保留状态目录，不影响 rdns 或 nanopc。若用 Apple Home 测试，只配对 `HomeSense Simulation`；生产迁移与真实设备控制仍未执行。
