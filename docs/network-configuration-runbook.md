# 华为核心、QNAP 与 AC 配置操作手册

版本 0.4，2026 年 10 月 13 日。**离线草案，未执行任何设备配置、DHCP 服务或无线服务启动。** 本文将[网络架构设计](network-architecture.md)转为操作步骤；代码块是未来配置窗口内的人工操作示例，不是自动部署脚本。接口占位符、地址冲突、实际固件和既有规则核查完成后，才能在隔离环境验证。

## 1 顺序、参数与停止条件

顺序为：备份与接口登记 → 隔离配置新核心 → 单台 QNAP 验证标签和管理入口 → AC/AP 管理与业务 SSID → 边界回程、DHCP、双栈分流验证 → 核心切换 → 分台迁移 .0.x 设备 → VLAN100 转维护模式 → 维护无线/DHCP 验收。

新核心准备期间不连接生产 LAN，不同时运行与旧核心相同的 `.253` 网关或 OSPF Router ID。已有 AP、QNAP、边界配置的变更应分设备执行，不能一次覆盖整网；遇到管理失联或前置能力不支持时，恢复该设备备份再处理。

| 参数 | 规划值 / 核查要求 |
| --- | --- |
| 核心 | S5735S-L24PN4XE-A3；实际固件及 CLI 接口名待登记 |
| 业务 VLAN | 1、3 → nanopi5/WAN A；2、4 → nanopc/WAN B；5 → AC/AP 管理，公网按需走 B |
| IPv4 核心网关 | 192.168.1.253、192.168.2.253、192.168.3.253、192.168.4.253、192.168.5.253，各 /24 |
| 维护 VLAN | 100，192.168.0.0/24；迁移期保留现有 .0.253，迁移完成后日常只有二层维护，临时核心 DHCP 窗口须启用受限 SVI |
| 边界 LAN | nanopi5 .3.1；nanopc .4.1；两端 WAN 默认路由仍指向各自 WAN |
| AC | TL-R489GP-AC，192.168.5.1/24，目标网关 .5.253；仅用 LAN 口连接 QNAP A |
| ngr / NAS | 保留现有 VLAN3/4 地址，先核查再填；本手册不重配服务器网络或接管设备 |
| QNAP 管理 | 先保留现有管理地址/可达路径；只有管理 VLAN5 能力经验证后才迁移 |
| 维护 AP | 核心电口17对应吸顶 AP，记录 MAC/序列号/安装位置；管理 VLAN5 |
| DHCP 所属 | VLAN1/2/100：核心；VLAN3/4：各自 systemd-networkd DHCPv4、rdhcpd DHCPv6/PD/前缀 RA |
| 维护 DHCP 示例 | 核心临时 SVI .0.254/24，静态保留维护电脑 .0.200/24；静态租约无限期，须主动关闭服务与释放地址 |

先登记下列占位符，不猜测面板序号与 CLI 的映射：

| 占位符 | 对应实际接口 |
| --- | --- |
| `<CORE_E17>` | 电口17，维护 AP |
| `<CORE_E18>` 至 `<CORE_E22>` | 其余5台 AP |
| `<CORE_E23>` / `<CORE_E24>` | nanopi5 / nanopc LAN |
| `<CORE_X1>` / `<CORE_X2>` | QNAP A / B 万兆上联 |
| `<CORE_X3>` / `<CORE_X4>` | NAS VLAN3 / VLAN4 万兆口 |
| `<ROOM_PORT>` | 某条房间线，实际角色决定 Access VLAN |
| `<MAINT_WIFI_MAC>` | 维护电脑在维护 SSID 使用的实际 MAC（核对随机 MAC） |

前置核查包括：QNAP管理流量实际 VLAN、旧 AC 与 Wi-Fi7 AP 兼容性、指定 AP 发布维护 SSID、AP 管理帧是否带标签、IPv4/IPv6 PBR 同入口共存方式、现有聚合模式。未确认时不将候选配置当成已验证脚本。

## 2 华为核心

### 2.1 导出与建立二层 VLAN

未来在配置窗口通过 Console 登记并导出：

```text
display version
display interface brief
display current-configuration
display saved-configuration
display vlan
display ip routing-table
display ospf peer
display ipv6 routing-table
```

备份文件保留在私有运维目录，不把带凭据的导出加入仓库。新核心独立配置 VLAN：

```text
system-view
vlan batch 1 2 3 4 5 100
```

华为 V600 官方示例使用 `port link-type`、`port trunk pvid vlan` 与 `port trunk allow-pass vlan`；本文将该语法映射到本网络，实际接口及型号限制仍按到货版本确认。[官方接口示例](https://support.huawei.com/enterprise/zh/doc/EDOC1100459020/f9553938)

### 2.2 房间、路由器与 NAS：Access

每条房间线逐条登记为3（A）或2（B），不向普通终端开放4；专用 A组网关线按1，不能把所有房间口统一批量改成一个 VLAN。以下以一条 A 组房间线为例：

```text
interface <ROOM_PORT>
 port link-type access
 port default vlan 3
 quit
interface <CORE_E23>
 port link-type access
 port default vlan 3
 quit
interface <CORE_E24>
 port link-type access
 port default vlan 4
 quit
interface <CORE_X3>
 port link-type access
 port default vlan 3
 quit
interface <CORE_X4>
 port link-type access
 port default vlan 4
 quit
```

B组房间口填写另一条已登记的实际接口，使用 VLAN2；不要将下面占位符替换为已接路由器或 NAS 的接口：

```text
interface <ROOM_PORT>
 port link-type access
 port default vlan 2
 quit
```

端点按无标签配置配套；若 NAS/路由器已有802.1Q标签，先协调移除标签或保留正确 Trunk，不能两端模式不一致。NAS 两口独立地址，不建立跨 VLAN LACP。

### 2.3 AP：管理5，业务3/2，维护100按需

本文选用 **AP管理流量不带标签、业务 SSID 带标签** 的候选模式：AP口 PVID5，5出方向无标签，3/2带标签。如果 AP 已配置管理 VID5 并给管理流量打标签，不套用此候选模式；改为两端一致的 Tagged5，并在隔离测试中验证 AP上线。

```text
interface <CORE_E17>
 port link-type trunk
 port trunk pvid vlan 5
 undo port trunk allow-pass vlan 1
 undo port trunk allow-pass vlan 4
 port trunk allow-pass vlan 2 3 5
 quit
```

电口18–22逐口使用同样的管理/业务配置，保持自动协商并验证 PoE供电。日常不放行100；只在维护窗口向电口17增加100。AP若确实需要1业务 SSID，再放行该 VLAN；B组 SSID使用2，不发布4，不默认给每台 AP全部 VLAN。

### 2.4 QNAP上联：业务标签与管理入口分开核对

以下是 **QNAP管理 VLAN5 经验证可用、两端以未标记5承载管理** 时的候选配置：

```text
interface <CORE_X1>
 port link-type trunk
 port trunk pvid vlan 5
 undo port trunk allow-pass vlan 4
 port trunk allow-pass vlan 1 2 3 5
 quit
interface <CORE_X2>
 port link-type trunk
 port trunk pvid vlan 5
 undo port trunk allow-pass vlan 1
 port trunk allow-pass vlan 2 4 5
 quit
```

此时 QNAP A上联的1是带标签业务 VLAN；不能因为 VLAN1是缺省成员就误将家庭网关流量设为未标记。`allow-pass` 会保留已有其他允许成员，须用 `display current-configuration interface <接口>` 逐条核对并撤销不需要的旧成员，不使用 `allow-pass vlan all`。

**若 QNAP管理 VLAN5 未验证，不执行上述 PVID切换。** 先按第3.3节保留其原生管理通道。管理 VLAN/PVID和对应 Untagged 成员须两端一致；如管理5带标签，则按实测改为 Tagged5，不同时套用未标记5方案。

### 2.5 五个业务 SVI、OSPF与出口

在隔离新核心准备五个业务网关，旧核心仍在线时不要连入相同广播域：

```text
interface Vlanif1
 ip address 192.168.1.253 255.255.255.0
 quit
interface Vlanif2
 ip address 192.168.2.253 255.255.255.0
 quit
interface Vlanif3
 ip address 192.168.3.253 255.255.255.0
 quit
interface Vlanif4
 ip address 192.168.4.253 255.255.255.0
 quit
interface Vlanif5
 ip address 192.168.5.253 255.255.255.0
 quit
```

迁移期还须移植原 Vlanif100 `.0.253/24`、旧设备实际经过的二层链路、必要 DHCP服务/中继、专用回程与访问规则；不按下面最终维护模式提前删除它们。

OSPF目标为进程1、RID10.0.0.6、area0，只有Vlanif3/4建立邻居。以下为候选骨架，需合并旧认证、网络类型、优先级和重分发规则；迁移期额外保留原 VLAN100的发布：

```text
ospf 1 router-id 10.0.0.6
 silent-interface all
 undo silent-interface Vlanif3
 undo silent-interface Vlanif4
 area 0.0.0.0
  network 192.168.1.253 0.0.0.0
  network 192.168.2.253 0.0.0.0
  network 192.168.3.253 0.0.0.0
  network 192.168.4.253 0.0.0.0
  network 192.168.5.253 0.0.0.0
  quit
 quit
```

静默业务接口仍发布直连网段但不建立邻居；语法参考[华为 OSPF命令说明](https://support.huawei.com/enterprise/zh/doc/EDOC1100352131/753f0155)，该参考不是本型号的实际固件验收。默认出口沿用经 nanopc `.4.1` 的既有外部默认路由，不擅自新增同优先级默认路由，也不让边界 WAN默认指回核心。

随后依次完成：

1. 按架构第7节准备两边的IPv4回程和NAT：A纳入1/3，B纳入2/4/5；迁移期继续保留0网段既有规则。
2. 按第2.6节配置 DHCP：VLAN1/2 核心地址池；VLAN3/4 保留 systemd-networkd，Router 改为核心 .253；VLAN100 临时池不常驻。检查 Option121，不改变 `.3.7` 的保留记录。
3. 按架构第4节定义IPv4 PBR：先内网目的豁免，再单机例外，最后1/3→A、2/4/5→B。迁移期补入0网段豁免，避免访问旧设备绕公网。
4. 按架构第5节核查自动 PD：A侧直接使用 VLAN3 的 LAN /64；B侧 nanopc 的 rdhcpd 从 /60 池给核心 Vlanif4 委派 /64，用于 Vlanif2。VLAN1 保持 IPv4，不新增 PD 绑定或 RA；旧 ng3prefix 引用先核查。旧核心 /64→/64 已生效，目标 V600 指南存在绑定长度限制，新核心兼容性验收前不启用目标绑定与下行 RA。3/4 则要求 rdhcpd 的 RouterLifetime=0、核心默认路由 RA 不含 PIO；该能力已在 rproxy 本地实现，须在隔离环境验收后另行部署。5/100 不启用 IPv6。
5. **先验证第6节双栈策略绑定和ACL/PBR硬件下发，再绑定业务入口。** 不直接连续执行两条 `traffic-policy` 并假定共存；硬件资源或第二策略覆盖第一策略时停止上线。
6. 安全规则先明确ngr实际来源到`.1.10/.2.11:50007`及回复、管理终端到网关管理页、AP到AC管理流量、必要DNS/DHCP/ND/ICMPv6。配对不是全网放行；只有经过验证的完整规则集才启用隔离。

新核心通过隔离验收后，断开旧核心再接新核心，逐项验收邻居、回程、单栈/双栈出口、终端网关与PoE。不要在本次网络验收中顺带启用HomeSense真实控制。验证配置后执行 `return`、`save`，按提示确认；保留改动前备份和实际命令记录。

### 2.6 DHCPv4、PD 与 RA 职责

A组普通终端可直接进入 VLAN3，保留其 systemd-networkd DHCPv4 与 rdhcpd 前缀服务；B组普通终端全部进入 VLAN2；VLAN4 只用于 nanopc、NAS及单独登记的 ngr 服务器链路，不开放房间/桌面/AP客户端。VLAN2 到 NAS/服务器的访问通过核心路由与受限 ACL；保持其核心 DHCPv4 与 RA。HomeSense 客户端验收入口改为3/2，ngr 的3/4服务器接入不变，VLAN2的发现需要受控跨网段发布/转发验证。

| VLAN | DHCPv4 | IPv6 |
| --- | --- | --- |
| 1 | 核心全局池，网关 .1.253 | 保持 IPv4，不启用 IPv6/RA/PD绑定 |
| 2 | 核心全局池，网关 .2.253 | 核心经4申请 ng4prefix PD 后用于2并发 RA；新核心兼容性验收前停用 |
| 3/4 | nanopi5/nanopc 的 systemd-networkd | 各自 rdhcpd 管理 DHCPv6/PD 和 PIO；核心另发默认路由 RA |
| 5 | 保留核对后的唯一 AP 管理地址服务 | 首期停用 |
| 100 | 原设备迁移完成后日常关闭；核心临时维护池 | 停用 |

核心业务池见 [VLAN1/2 DHCP 模板](../deployment/network/huawei-dhcp-vlan1-vlan2.cli.example)。先填写实际 DNS 与 .1.10 的 MAC，核查动态范围、保留地址及旧 DHCP 关闭窗口，再按实际固件验证 `dhcp enable`、`ip pool`、`dhcp select global`。两个目标网关保留 .1.10、.2.11，不把记录写入 VLAN3 的池。VLAN1/2 静态绑定租约为无限期；其他终端示例租期1天。

VLAN3/4 只合并以下内容到各自当前生效 `.network.d/*.conf`，不覆盖主文件、动态池、DNS、Option121 或 `.3.7` 保留：

```ini
# nanopi5，VLAN3 DHCPv4
[DHCPServer]
EmitRouter=yes
Router=192.168.3.253
```

```ini
# nanopc，VLAN4 DHCPv4
[DHCPServer]
EmitRouter=yes
Router=192.168.4.253
```

部署窗口核对 systemd 版本和最终合并配置，逐一处理既有 Option121 默认路由及过时 AC 主机路由；只给服务器改 Router 不会立即改变已获租客户端，须验证重新获租后的路由。DNS 地址仍是实际解析服务，不能自动改成 .253。[systemd v257 官方说明](https://github.com/systemd/systemd/blob/v257/man/systemd.network.xml)

WAN /60 由边界 WAN DHCPv6 客户端申请，rdhcpd 管理下游池，不负责运营商拨号侧 PD 请求。核对两边池来源、LAN /64 排除、独立租约文件、核心 DUID、请求接口和实际回程路由。自动获租可能改变下游 /64，须同步内网目的 ACL、跨线路回程及防火墙；现有固定地址示例不是自动同步实现。

PD 客户端和 RA 分工的限制见架构第5节：旧核心已确认上行 /64 获租和 VLANIF2 的 /64 地址生成，目标新核心的 /64→/64 绑定仍需到货固件验证，不把 `--slice-length 63` 当作可直接使用的替代。VLANIF1 保持 IPv4，本次不绑定 ng3prefix。两个上行接口此前快照均为 REBIND；当时源码未处理续租是候选原因。rproxy `63f464b` 已修复 Renew/Rebind，并实现 RA RouterLifetime=0；原有静态 IA_NA 另提交 `d95c3a9`。独立修复快照 28 项测试通过，合并静态租约后 31 项通过，严格 Clippy、格式及 Linux ARM64 编译检查通过；这些是本地证据，现网运行版本未更新，真实续租未验收。

未来部署窗口只将以下参数合并到 nanopi5/nanopc 各自原有 rdhcpd 启动参数，不覆盖接口、池、租约文件或 DNS；本次不执行服务修改、重载或重启：

```text
--ra-router-lifetime-secs 0
```

先验证核心在3/4发出的默认路由 RA（RouterLifetime>0、不含 PIO），再切换边界 lifetime=0；边界仍须发布各自 LAN /64 PIO 和真实 DNS。M/O 标志按实际 SLAAC/有状态 DHCPv6 服务统一，lifetime=0 不自动关闭地址服务。抓包确认两类 RA 的来源、PIO、M/O 和有效期，并在客户端核查只有核心默认路由。续租验收核对成功 Reply、前缀不变、租期刷新、PD 回程及重启重载；同时核对两线路源出口、上游剩余授权期和策略同步。Release、自动 PD 路由回收及上游前缀动态跟踪本次未新增，相关验收不可标为通过。Linux 内核路由更新、真实 UDP/RA 收发及新核心兼容性仍是部署门槛。

## 3 两台 QNAP QSW-M408-4C

### 3.1 QSS页面与端口矩阵

QNAP官方对本型号的 VLAN操作入口为 **Configuration → VLAN**，在各 VLAN里选择端口 Tagged/Untagged并保存；不是另找一个名为Trunk的总开关。[QNAP Trunk配置说明](https://www.qnap.com/en-ca/how-to/faq/article/how-can-i-configure-trunk-mode-on-a-managed-switch)

表中T=Tagged，U=Untagged，未列出者为非成员。Combo1/2对应物理面板9/10的候选编号，实际以页面为准。**主表前提是QNAP管理5已验证**，PVID5是对应管理入口的未标记帧归属；若不支持独立PVID输入，应核对实际Untagged成员决定的入方向分类。

| QNAP A端口 | 用途 | 日常成员 | PVID/未标记归属 |
| --- | --- | --- | --- |
| Combo1（光） | 核心X1 | T1、T2、T3、U5 | 5 |
| Combo2（电） | 桌面 SR-ST3005 | T2、T3、U5（桌面未标记管理5经验证后） | 5，两端匹配；未验证时保留原管理通道 |
| 千兆1 | AC LAN | U5 | 5 |
| 千兆2、3 | ngr 聚合A | U3 | 3，两口及逻辑组一致 |
| 千兆4 | 家庭网关 | U1 | 1 |
| 千兆7（维护时） | 单台待配置设备 | U100 | 100；原占用情况先核查 |
| 千兆8（可选维护口） | 有线维护电脑 | U100 | 100；原占用情况先核查 |

| QNAP B端口 | 用途 | 日常成员 | PVID/未标记归属 |
| --- | --- | --- | --- |
| Combo1（光） | 核心X2 | T2、T4、U5 | 5 |
| 千兆1、2 | ngr 聚合B | U4 | 4，两口及逻辑组一致 |
| 千兆3 | PE11 | U2 | 2 |
| 其他端口 | 保留已登记设备 | 按原角色逐口核对 | 不整机恢复出厂 |

维护时只给经过QNAP A的链路增加T100；QNAP B不参与维护时不用增加100。若原来有`.0.x`设备在任一QNAP，迁移完成前保持其Access100及必要上联100；上表是迁移后的日常目标。

实际设置顺序：

1. 导出/记录固件、管理IP/网关、每口 VLAN成员及聚合，保持一个已知可登录的本地管理口。
2. 创建所需 VLAN；先增加新成员，验证管理路径后再删除旧成员。业务Access口移除无关Untagged成员，确保收发和PVID一致。
3. 先改一端、使用本地维护电脑确认可达，再匹配核心/桌面另一端；不凭管理IP的网段推断其内部管理VLAN。
4. 各次点击Save后读回成员和端口状态；完成稳定配置后按实际固件导出配置或截图。支持配置导出的用原生导出，页面没有时不虚构菜单。

### 3.2 聚合与管理保护

若现有ngr聚合为LACP，在 **Configuration → Link Aggregation**（页面名称依固件）分别建立A的2/3、B的1/2，保持服务器模式一致；若原模式不是LACP，先保持原模式，不单方面切换。逻辑LAG及成员的VLAN应一致，禁止跨两台QNAP组成同一组。[QNAP本型号功能规格](https://www.qnap.com/en-us/product/qsw-m408-4c/specs/hardware)

每台单线上联核心，Combo电/光二选一。RSTP/环路保护保持既有已验证设置；若调整生成树根桥优先级，统一让核心为根，不在迁移时随意改变多项链路协议。管理ACL规则先确保当前运维来源可访问，再实施限制；此处不提供会覆盖既有ACL的整表配置。

### 3.3 QNAP管理 VLAN5未验证时的分支

官方功能资料确认业务VLAN与LACP，但本轮未取得这两台设备实际固件的管理VID设置证据，故 **不承诺QSS管理可以迁到5**。先保留现有管理地址/原生VLAN及本地管理口，在隔离环境验证新的管理路径。

若实测管理固定在原生VLAN1，可在既有五业务VLAN内采用受限管理分支：两台管理地址选VLAN1内不冲突的保留地址，核心与各QNAP上联都采用U1/PVID1，其余业务2/3/5或2/4/5保持T成员。此时QNAP B的核心链路也须增加VLAN1，核心X2不得执行移除VLAN1的主表命令；只允许授权管理来源访问交换机管理IP。该分支须先验证管理CPU实际归属，不能将其当成所有固件通用的补救。

不要仅把管理IP改为`.5.x`却继续让管理流量留在1，这会失联。无法确认管理通道时保持原管理路径，不下发主表；不新增第七个业务VLAN，也不依赖维护100维持日常交换机管理。

## 4 TL-R489GP-AC与AP

### 4.1 保留AC功能并旁挂

1. 备份AC及AP配置、SSID/密钥、硬件/固件和AP名单；记录哪个LAN口用于管理连接，不恢复出厂或删除现有无线服务。
2. AC以LAN口接QNAP A千兆1（U5）；地址`.5.1/24`、目标网关`.5.253`，DNS填写实际可达解析服务。WAN口不接到这个LAN链路，不为本方案再拨号/NAT。
3. 原AC若承担DHCP，先确定其实际服务对象。AP管理地址池可在AC或其他服务器中择一保留，但Router必须为`.5.253`；不能直接关闭唯一AP地址服务，也不能开启第二个同网段服务。业务3/2与维护100不由AC额外发租约。
4. AP管理采用第2.3节约定的未标记5或已核对的Tagged5模式，先验证一台AP上线；其他AP逐台确认。跨网段发现不是本方案默认前提，AC/AP处于同一管理VLAN。

### 4.2 业务无线与维护无线

官方R系列资料描述 **AP管理 → AP设置** 和 **AP管理 → 无线服务设置**，后者有绑定VLAN、自动绑定全部AP等字段；资料示例是其他R型号，TL-R489GP-AC的实际页面及新AP支持仍须确认。只将字段映射为本方案，不照抄官网示例中的删除默认服务或示例密码。[TP-LINK配置示例](https://media.tp-link.com.cn/document/articles/4491.html)、[SSID绑定VLAN说明](https://smb.tp-link.com.cn/service/detail_article_2304.html)

| 无线服务 | VLAN | AP范围 | 日常状态 |
| --- | --- | --- | --- |
| 现有A组SSID（保留名称） | 3 | 需提供A业务的AP | 启用 |
| 现有B组SSID（保留名称） | 2 | 需提供B业务的AP | 启用 |
| Zhiju-Maintenance（可更名） | 100 | 仅电口17对应AP | 禁用，维护时开启 |

具体步骤：

1. 检查现有服务的绑定VLAN，分别调整A=3、B=4，保持本地/分布式转发；不启用AC隧道转发替代已确定的方案。
2. 新增独立维护服务：SSID=`Zhiju-Maintenance`，VID=100，强PSK；优先实际固件支持的WPA2-AES或兼容WPA3配置，不写真实密钥进仓库。
3. 关闭“自动绑定所有AP”，只选择已登记的维护AP/所需射频，状态保持禁用。如果只能全局发布，先记录能力缺口，不直接把100放行到所有AP。
4. 关闭会阻断维护电脑与目标设备必要流量的隔离/访问限制；不要因此取消其他SSID的隔离。若用了MAC白名单，登记电脑在这个SSID上实际使用的Wi-Fi MAC，注意随机MAC变化。
5. 保存并读回绑定范围、VID和禁用状态。以后维护窗口仅开启该服务，结束后关闭；AP管理仍在5，无须改变AP管理地址或整个AP上联PVID。

## 5 VLAN100从迁移期转为维护期

确认`.0.10→.1.10`、`.0.11→.2.11`及所有其他`.0.x`设备迁移，HomeSense连接与其他依赖已更新、业务验证完成后，才执行本节。**迁移未完成时保留现有`.0.253`网关、专用回程、DHCP及访问规则，不同时开启临时维护DHCP。**

未来人工操作需按实际配置精确撤销：

1. 关闭Vlanif100三层功能，保持VLAN100二层定义。候选命令为 `interface Vlanif100` → `shutdown` → `quit`；核对接口确实不再转发、路由不再存在。需要删除SVI时另行采用该版本支持的命令，不使用 `undo vlan 100` 删除维护VLAN。
2. 停止原VLAN100 DHCP服务/中继及地址池，停止已有RA（若曾开启），撤销OSPF中该网段的network/import路径以及两边专用静态回程、NAT、PBR与过时访问规则。不能以一条`undo network`覆盖所有发布来源。
3. 核查核心及两台边界不再有该/24的专用可达路径；共享的`.0.0/16→NULL0`按原用途检查，不盲目删除。五业务VLAN的路由、DHCP、NAT和RA保持正常。
4. 将原Access100业务口恢复规划用途或关闭；从不需要的Trunk撤销100，只保留VLAN定义。维护SSID保持禁用，不配置常驻维护默认网关或NAT。

## 6 一次无线维护的完整操作

### 6.1 打通临时二层链路

以目标设备在 QNAP A、维护电脑使用无线入口（可选有线口8）为例。先记录千兆7/8原配置并确认可临时使用：

| 位置 | 维护期间 |
| --- | --- |
| 核心E17 → AP | 既有PVID5与业务成员保持，增加Tagged100 |
| 核心X1 → QNAP A Combo1 | 既有管理/业务成员保持，两端增加Tagged100 |
| QNAP A千兆7 → 目标设备 | 临时U100/PVID100，移除该口其他业务成员 |
| QNAP A千兆8 → 有线维护电脑（可选） | 临时U100/PVID100，移除该口其他业务成员 |
| AC维护SSID | 仅指定AP，VID100，开启 |

华为临时增加100的示例：

```text
system-view
interface <CORE_E17>
 port trunk allow-pass vlan 100
 quit
interface <CORE_X1>
 port trunk allow-pass vlan 100
 quit
```

QNAP在VLAN100行将Combo1选为T、千兆7/8选为U并保存，核查PVID。AC所在QNAP千兆1仍U5，不加入100；本地转发的维护数据不需要经过AC口。目标默认地址重复时一次只接一台。

### 6.2 核心临时 DHCP：隔离通过后才启用

使用 [华为 VLAN100 临时 DHCP 模板](../deployment/network/huawei-vlan100-maintenance.cli.example)，不启动 Linux DHCP 或 dnsmasq。它是候选 CLI：需要临时 SVI `.0.254/24` 以匹配全局池，窗口内会有 `.0.0/24` 直连路由。日常仍关闭 SVI；维护不恢复旧 `.0.253` 网关或公网出口。

1. 仅在所有旧 .0.x 迁移后准备；核查没有既有 VLAN100 DHCP/中继，登记池名、ACL编号与接口原配置。
2. 核查 `.0.254`、`.0.200` 以及待维护设备默认地址无冲突；默认地址相同的设备一次一台。填写维护无线实际 MAC。
3. 候选 ACL 只允许维护与核心间必要 DHCP，其他经 SVI 的 IPv4 流量拒绝；二层电脑到目标的管理流量应保留。必须在隔离环境验证 VLANIF 入/出绑定实际支持、发往 CPU 的 DHCP 被允许、核心本机其他管理入口被限制、业务到100及100到业务/公网均阻断。不以地址池排除替代 ACL，不使用未验收规则开启 SVI。
4. 专用全局池仅 `.0.200` 可用并静态绑定该 MAC，其余地址排除；这不是防 MAC 伪造认证。静态租约无限期，示例 `lease` 的15分钟只适用于动态分配，不能作为关闭服务的替代。
5. 模板不配置网关、DNS、Option121/249，但官方默认行为须用实际 OFFER/ACK 验证。若仍下发 Option3，或 ACL 无法可靠隔离，保持 SVI/DHCP 关闭、维护电脑手动设置 `.0.200/24` 且无网关，沿二层无线入口维护；不编造取消 Option3 的命令。

配置语法依据 [全局地址池](https://support.huawei.com/enterprise/zh/doc/EDOC1100459025/b4befc01) 与 [接口绑定](https://support.huawei.com/enterprise/zh/doc/EDOC1100459025/c9c90844)，接口须有 IPv4 地址；模板尚未经到货设备解析或包级验收。无 relay，不发布该网段到 OSPF，也不添加边界回程/NAT/PBR。

### 6.3 无线客户端与包级验收

维护电脑重新获租后应为 `.0.200/24`，该接口无默认路由、DNS、Option121/249。其他业务接口不能桥接至维护接口。未来可在客户端抓包（接口名填实际无线接口，本次不执行）：

```sh
sudo tcpdump -ni '<MAINT_WIFI_INTERFACE>' -vvv 'udp port 67 or udp port 68'
```

核对唯一服务的 DISCOVER/OFFER/REQUEST/ACK、服务器 `.0.254`、客户 MAC、Subnet Mask 及选项；另在核心检查实际地址池租约和计数，命令参数按固件确认：

```text
display ip pool name HS_MAINT100
display dhcp server statistics
display ip routing-table
```

确认未登记 MAC 无可用租约、维护电脑可同 VLAN 访问目标管理页，而跨 VLAN 与公网被阻断；检查核心本机管理面限制。只读取管理页/MAC，不开启 TCP485 透传或发送控制。手机不能因移动数据访问成功而误判维护网有互联网。

### 6.4 结束与恢复

把目标移回业务 VLAN、验证新地址，关闭维护 SSID。仅解绑维护 DHCP 并关闭 SVI100，不执行全局 `undo dhcp enable`，否则会影响 VLAN1/2：

```text
system-view
interface Vlanif100
 shutdown
 undo dhcp select global
 quit
```

按实际固件删除本轮专用池 `HS_MAINT100` 或保持未绑定状态，撤销本轮 ACL 绑定和规则，恢复原配置；先确认没有其他引用。维护电脑释放租约、恢复原 SSID/地址；静态租约不会15分钟自动过期。恢复 QNAP7/8 的成员和 PVID，再撤销本轮新增标签：

```text
system-view
interface <CORE_E17>
 undo port trunk allow-pass vlan 100
 quit
interface <CORE_X1>
 undo port trunk allow-pass vlan 100
 quit
```

只撤销本次新增且不被其他维护链路使用的成员。核查 SVI100 关闭、`.0.0/24` 直连消失、无常驻 DHCP/RA/出口，AC 管理仍在5、各业务 DHCP 和路由正常。不把临时 SVI 开启状态保存为开机默认状态。

## 7 未来验收记录与回退

| 阶段 | 必须记录的通过条件 | 当前状态 |
| --- | --- | --- |
| 核心基础 | 房间与桌面 A口3/B口2；五业务SVI、PoE、QNAP/NAS 10G、边界2.5G | 未实机验收 |
| QNAP | T/U/PVID两端一致；管理通道确认；聚合模式一致、单线断链可恢复 | 未实机验收 |
| AC/AP | 管理5可达、A组SSID→3/B组SSID→2、客户端无VLAN4地址、单AP维护SSID独立启停 | 未实机验收 |
| rdhcpd 本地源码 | Renew/Rebind 保持绑定并刷新租期；RA lifetime=0 保留 PIO/DNS；旧租约兼容、错误路径不发布新内存租约 | 独立修复28项、合并静态租约31项通过；严格Clippy、格式与Linux ARM64编译通过；未部署、未做真实收发/内核路由验收 |
| 双栈 | VLAN1保持IPv4、VLAN3直接接入、VLAN2的PD兼容性验收，OSPF邻居Full、rdhcpd默认路由RA关闭、动态租约到回程/策略同步、双策略硬件有效 | 存在未解除门槛，未实机验收 |
| HomeSense发现 | 单实例在实际客户端3/2发现并可达；服务器3/4地址回程、受控mDNS发布/转发及权限有效 | 待隔离验收，旧3/4模拟结果不替代 |
| 迁移门槛 | .0.x全量盘点并迁移，依赖与连接配置已更新 | 尚未完成，保留现网100三层服务 |
| 维护 | 临时 SVI 的 DHCP、双向及 CPU 隔离验收；包无网关/DNS/路由选项，静态绑定正确 | 未启动、未验收 |
| 恢复 | SSID/DHCP/临时标签关闭，端口和客户端恢复，100网关仍关闭 | 待未来维护结束核查 |

核心切换失败时隔离新核心，恢复旧核心及原布线/网关/边界配置；不能同时启用重复`.253`和RID。单台QNAP或AP变更失败先恢复该设备原VLAN和管理配置，不重置所有设备。VLAN100迁移未完成时，不将最终关闭步骤当成故障处理；维护阶段异常则先停临时DHCP/SSID、恢复端口，不擅自改现网DHCP或HomeSense真实控制。

## 8 本轮核查范围

已核对文档中的 VLAN、端口、DHCP 所属与迁移前后状态。此前查阅官方资料，并通过 SSH 只读查看两台边界的 rdhcpd 参数、租约和 IPv6 路由；核心配置和地址状态来自用户提供的 CLI 输出。2026-10-13 另完成 rproxy 续租与 RA 源码修复、独立提交和本地验证，本手册同步记录参数、证据与待验收项。华为 CLI、临时池实际选项、ACL 对 VLANIF/CPU 的覆盖、QSS 管理 VID、旧 AC/AP 兼容性、双栈硬件转发均待实际版本隔离测试；旧核心 /64 PD 到 VLAN2 的 /64 地址生成已实证，目标 V600 指南仍有限制、须到货验证。rdhcpd RouterLifetime=0 的实现和本地报文测试已通过，实际部署、PD 续租和双 RA 分工仍未验收。本次仅提交并推送代码/文档，没有下发设备配置、部署二进制、启动/重启服务、修改 SSID 或透传控制。Git 推送不代表已部署或通过实机验收。
