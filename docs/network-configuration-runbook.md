# 网络配置与维护手册

拓扑、端口和地址服务以[网络架构](network-architecture.md)为基线。本页说明维护与排障流程，不包含可直接重放的现场完整配置。真实 MAC、全球 IPv6 前缀、租约、PSK及管理凭据留在私有目录。

## 变更前核对

1. 登记实机型号、固件、接口与对端，保存运行／启动配置，准备独立管理入口与本次回退步骤。
2. 检查重复网关、DHCP／RA来源与二层环路，确认不会重新接通停用路径。
3. 明确本次设备、接口和权限范围，不连带改变 HomeSense 总线、SSID或其他 VLAN。
4. 核对接口模式、允许标签、PVID、PoE及实际供电；QNAP6号本地维护路径保留。
5. 管理失联、MAC抖动、错误计数增长、地址来源或路由隔离异常时停止并按本次备份回退。

华为只读命令按实际固件帮助使用：

```text
display version
display interface brief
display current-configuration
display saved-configuration
display configuration changes
display vlan
display ip routing-table
display ipv6 routing-table
display ospf peer
display ospfv3 peer
```

配置可解析、硬件策略生效、链路 Up 与实际客户端可达分别检查。

## 核心、QNAP与 AP

### 端口

| 核心口 | 模式 | 业务标签／PVID | PoE |
| --- | --- | --- | --- |
| 23，NanoPi5 | Trunk | Tagged3，PVID4090 | 关闭 |
| 24，NanoPC | Trunk | Tagged4，PVID4091 | 关闭 |
| X1，QNAP A | Trunk | Tagged2／3／100，Untagged5，PVID5 | 不适用 |
| X2，QNAP B | Trunk | Tagged2／4／100，Untagged5，PVID5 | 不适用 |
| 2，AC100 | Access | 5 | 关闭 |
| 5，R6S wan | Access | 3 | 关闭 |
| 6，R6S lan1／br2 | Access | 4 | 关闭 |
| 15–22，AP | Trunk | Tagged2／3／4／100，管理PVID5 | 按实际供电 |

现用485网关经QNAP A的8号Untagged100接入，核心10号是预留口。两台边界上联的隔离PVID不提供业务三层服务；X1／X2的PVID5用于管理网络，VLAN4092维护入口单独保留。

### QNAP

QSS中T表示Tagged、U表示Untagged，逐VLAN核对成员及PVID。同一Combo光／电侧不能同时占用，10号Combo是核心上联。NAS沿已登记路径使用，X3／X4使用前重新核对；不能按规划图直接迁线。

每次单项保存后读回成员、PVID、链路与错误计数，再验证本地维护入口。未经实机核对，不移动QSS管理CPU到另一VLAN。

两台 QSW-M408-4C／QSS 1.3.0 的管理接入采用 QNAP内部 VLAN1、核心侧 VLAN5：QNAP10号口设置 VLAN1 Untagged、PVID1，保留A侧 Tagged2／3／100与B侧 Tagged2／4／100；6号本地维护口保留 VLAN1 Untagged、PVID1。核心X1／X2已于2026-10-26设置 VLAN5 Untagged、PVID5并保存，移除原端口隔离VLAN4093／4094及VLAN1成员；两口10Gbps、CRC为0，双栈路由邻居为Full。用户修改两台QNAP后，已核对A／B管理地址为`.5.251`／`.5.252`、ARP与对应MAC正确，地址均处于现有DHCP排除区间；核心同网段和VLAN3源地址探测成功。管理Mac取得自动路由后，两台管理页均返回HTTP200。本次没有修改核心ACL；访问范围以现有策略为准。

同日用户修改两台QNAP后，HomeSense与实体按键出现无响应。核查时主服务健康端点为200、PID未变，两台485网关的IP连通、VLAN100的MAC／ARP及核心双栈邻居均正常；从主机到网关的新TCP握手也成功。实际失效的是原有网关TCP连接：仍显示ESTAB，但超过20分钟没有ACK，持续重传且发送队列积压。保留证据后仅清除已核对的旧连接四元组，现有服务自行重连，接收字节持续增长、发送队列归零，用户确认App和实体按键均恢复；未重启E2或网关，未发送人工总线查询或控制测试，X1／X2配置未回退。最初丢包的具体触发原因未证实。排障时需同时检查健康状态、TCP的ACK／重传／收发计数，不能把ping成功或ESTAB单独视为总线连接健康。此恢复动作应按现场PID和四元组重新核对，不复用历史端口号；连接清除工具即使返回0也需检查stderr并验证旧连接消失及新连接收发恢复。

### NanoPi VLAN4链路抖动与网关旧连接（2026-11-02）

墙面面板和App共同无响应时，NanoPi内核记录`lan1`在18:45:55–18:48:58发生26次物理Down／Up；核心6号口独立记录26次物理Down，最后恢复时间为18:49:14。两端时钟有十余秒差异，保留原始时间，不作秒级强行对齐。端口为`HOMESENSE_NANOPI_VLAN4`，PVID4，复查为2.5Gbps／Full。CRC7、Giants19、总输入错误26是从未清零的累计错误帧计数，其中总数为7+19；它们与26次链路断连不是同一计数，不能逐次对应。两次复查错误帧计数未增加。

链路恢复后，到485网关的旧TCP连接仍为ESTAB，但超过6分钟没有ACK、存在重传及发送队列积压；同时ping和新TCP空握手成功。18:52:37仅清除经核对的旧四元组，服务自行以新transport epoch重连，发送队列归零、实际反馈恢复，用户确认实体按键恢复。服务PID未变、未重启、未发送人工灯光控制或总线查询。

网卡为RTL8125B rev05、r8169，现场内核7.3.1；只读检查显示EEE配置开启但未协商生效，PCIe运行电源状态active，未见该故障窗口的PCIe AER／网卡复位错误。物理抖动具体来自跳线／接头、PHY还是驱动仍未证实；不能仅凭26次断连或相同错误总数认定硬件损坏。先核对现场插拔记录和更换跳线，再进行单项端口／协商对照；本次未改OS、驱动或网卡参数。

软件排查确认两个缺口：系统调用写超时不能限制内核队列中未获ACK的数据；原接收路径又将内核终止性`ETIMEDOUT`与普通轮询空闲混为一谈。修补增加TCP空闲保活（30秒开始、5秒间隔、3次探测），Linux单连接`TCP_USER_TIMEOUT=10000ms`，并将Unix原始TCP超时纳入已审计的连接恢复。10秒是未确认数据的TCP配置上限，不是包含重连、同步在内的端到端恢复承诺；保活为TCP层探测，不增加485查询。正常轮询`WouldBlock`、单模块反馈超时、500ms场景读取、控制批顺序及旧指令不跨连接重放的约束保留。修补的测试与部署状态见[NanoPi部署记录](../deployment/nanopi/README.md#网关tcp健康修补2026-11-02已部署)。

### 核心与NanoPi校时（2026-11-02）

故障排查发现核心时间偏快约15秒，`display ntp status`为unsynchronized、stratum16，未配置上游；NanoPi的systemd-timesyncd已同步，报告offset约−4.3ms，Mac与独立公网NTP测时也基本一致。本次只调整核心，不更改NanoPi时钟。历史掉线日志保留原设备时间，不按校时后的时钟改写。

核心配置`ntp unicast-server 203.107.6.88 source-interface Vlanif4 preferred iburst`，使用VLAN4出口；NTP服务器对外服务保持关闭，不开放新的NTP服务入口。读回为synchronized、stratum3，上游会话valid／sane，offset约0.74ms；配置已保存并读回。北京时间UTC+08:00保持。NanoPi仍由现有NTP客户端独立校时，HomeSense PID554、异常重启0、网关发送队列0；本次未重启HomeSense或操作灯光。

### AP与SSID

15–22号依次对应F00、F21、F12、F01、F10、F20、F02、F11。使用私有MAC表核对设备，端口描述可能滞后。管理VLAN5由核心DHCP池`vlan5_ap`供址，AC100`.5.1`只管理AP。

NGR使用VLAN3，NGBOSS使用VLAN2，NGX使用VLAN4。核对每个频段的绑定和AP上联标签，分别验证新关联、DHCPv4、IPv6地址／默认路由、DNS、公网双栈及隔离。漫游后保留旧地址不能证明该AP新接入正确；核对时关闭再开启终端Wi‑Fi，排除旧租约和漫游。

若SSID绑定与实际VLAN不符：

1. 保存该AP及自动绑定设置，抓取目标新接入流量并核对802.1Q标签。
2. 对照另一个正常SSID，区分上联放行与AP绑定生效问题，不改PVID掩盖错误。
3. 只重新下发明确范围的SSID／频段绑定；页面默认VLAN0时必须明确选择目标VLAN。
4. 恢复原自动绑定策略并读回，确认其他AP、密码和SSID未变，完成全局保存。
5. 新接入验证后清理临时抓包／ACL，保留首次失败和实际结果。

NGX无法获址时核对AP端口是否包含Tagged4，再检查NanoPC的地址服务及回程；不能仅因服务进程正常排除链路标签问题。

## 双栈路由与隔离

VLAN3↔4、VLAN2↔4允许双向IPv4／IPv6，VLAN2↔3普通访问双向拒绝，保留HomeSense指定地址TCP8444及回包。VLAN2公网经核心与NanoPC。允许与拒绝方向均须从正确源接口测试。

IPv4策略主要位于Vlanif2，IPv6网段对允许位于Vlanif4，Vlanif3附加隔离及Vlanif100来源限制单独核对。目标固件不能在同一traffic policy混合IPv4／IPv6分类器；不能用ACL deny条目冒充需要放行的例外。

维护参考模板：

- [网段对互通](../deployment/network/huawei-lan-mutual-access.cli.example)
- [RA DNS](../deployment/network/huawei-vlan2-ra-dns.cli.example)
- [双栈隔离基础](../deployment/network/huawei-b-vlan2-vlan4-isolation.cli.example)

模板须结合实际允许矩阵、precedence及当前前缀选择性使用，不能整份下发。保存前后比较受影响方向的应用记录及运行／启动配置。

VLAN3通过DHCP Option121取得`192.168.0.0/24 → 192.168.3.253`、AC`.5.1/32`、QNAP`.5.251/32`／`.5.252/32`经`.3.253`的路由，默认出口仍为`.3.1`。配置位于NanoPi5的`25-br-lan.network.d/60-ac-route.conf`，使用systemd-networkd的IPv4 DHCP服务；rdhcpd负责IPv6，不能因名称相似而修改其服务代替IPv4 DHCP。2026-10-26新增两台QNAP主机路由时，已备份原文件并设置独立回退，核对Option121完整解码保留原三条路由后重载，服务端健康通过；Mac续租的ACK包含全部五条路由，实际到两台QNAP的路由均为有线接口经核心`.3.253`，网页访问HTTP200，不依赖手动主机路由。VLAN100仅允许指定管理终端及已审核服务例外；固定租约与真实源地址不公开。VLAN2允许IPv4访问AC`.5.1/32`。

### IPv6前缀与DNS

1. PD变化时先按获批流程保护VLAN2 IPv6转发，不让旧ACL错误匹配新前缀。
2. 读取两侧有效PD、LAN前缀、租期、回程与链路本地下一跳，不从历史日志复制前缀。
3. 同步PIO、网段对允许及隔离ACL、DNS /128路由、防火墙、RDNSS、双侧AAAA和HomeSense附加监听。
4. 验证允许／拒绝方向、DNS、续租、前缀撤回、公网双栈和HomeSense域名入口，再恢复并保存。

RA优先级在真正来源修改：VLAN3由NanoPi5的rdhcpd使用`--ra-router-preference high`，VLAN2核心使用`ipv6 nd ra preference low`，核心Vlanif3不另启RA。接口选路还受终端服务顺序和具体路由影响。

VLAN2 RDNSS使用NanoPC当前VLAN4前缀加`::4:1`，DNSSL为`lab.vixal.xyz`，寿命1800秒。NanoPC的LAN Token、DNS通告、两侧AAAA及核心回程使用相同地址。修改networkd前记录有效PD及不可达委派路由，不能只恢复地址文件就推定前缀路由恢复。

旧PIO只有在客户端迁移后撤销；持续通告会刷新其有效期。当前前缀同步是人工维护流程，字面ACL、DNS与socket不会自动跟随PD。

### Mac双网与DNS排障

区分全局路由、接口作用域路由、实际源地址和系统DNS选择，分别查看：

```sh
route -n get 192.168.4.1
netstat -rn -f inet
scutil --dns
```

双网切换后若公网TCP可用但域名失败，核对是否存在指向旧接口／旧源地址的DNS主机路由。仅在目的、网关和作用域与残留项完全一致时删除相应项，不清空路由表；记录本次变更以便恢复。

使用不强制绑定接口的新连接跨越拔线／插回测试，同时记录实际IPv4／IPv6出口、A／AAAA查询和HTTPS结果。强制绑定可用于定位，但不能作为系统自动选路通过的依据；新连接成功不保证既有连接无缝迁移。

## HomeSense入口与冷备

VLAN4 IPv6 回程由 R6S 的`24-br2.network.d/60-homesense-ipv6-return.conf`维护，专用表204／优先级10404仅匹配服务器 VLAN4 地址的/128；本地前缀直连，其他目的经 NanoPC 的链路本地地址，让它的连接跟踪看到完整往返。变更先备份并设独立回退，重载networkd后分别从有线VLAN3和Wi‑Fi VLAN2验证四个HTTPS入口，核对主服务PID／重启次数和网关连接未变。前缀变化时同步替换From与直连前缀，保留既有隔离规则。

独立R6S的两张网卡使用`.3.3`和`.4.4`，不桥接两网。域名`homesense.lab.vixal.xyz`指向`nanopi.lab.vixal.xyz`，两侧DNS发布对应A／AAAA；HTTPS／WSS在TCP8444提供同一服务。

前缀变化同步维护[附加监听](../deployment/nanopi/web/README.md)及精确回包规则。双网接收与发送可能走不同接口，核对真实流量再调整，不能放宽到无关内网。HAP使用单座原身份桥，不能通过同时启动NGR来测试可达性。

NGR只接收[定时数据备份](../deployment/nanopi/backup/README.md)，服务禁用、主角色关闭。恢复须先停止或可靠隔离旧主，再按[NanoPi恢复流程](../deployment/nanopi/README.md#手动恢复边界)处理。

## VLAN100维护与恢复

2026-11-03，本机管理Mac改用Wi-Fi时，访问485网关失败。已核对Option121路由正确、网关及HomeSense正常，原因是核心只允许原有线固定地址，而Wi-Fi使用另一地址。现已新增Wi-Fi单设备例外，与原有线例外并存；真实MAC／源地址与完整变更、回退记录保留在私有`20261103-m4max-wifi-vlan100`证据目录。

| 配置位置 | 本次持久变更 |
| --- | --- |
| NanoPi5 `/etc/systemd/network/25-br-lan.network.d/83-m4max-wifi-static-lease.conf` | 新增`DHCPServerStaticLease`，将该Wi-Fi MAC绑定到其原先正在使用的VLAN3地址 |
| 原`81-m4max-static-lease.conf` | 保留原有线MAC与独立固定地址，不与Wi-Fi共用地址 |
| 核心ACL3990 | 新增rule6，仅匹配Wi-Fi固定源地址`/32`至`192.168.0.0/24`；原rule5有线例外及其他规则保持 |
| 核心`V100_OUT4`／Vlanif100 | 保留原许可、拒绝分类器及出方向绑定，新增规则已保存并从启动配置读回 |

DHCP变更前备份现有文件，设置独立回退后只新增租约文件，通过networkd reload／reconfigure加载；原DHCP、Option121路由和其他租约文件哈希均保持。核心变更也设置独立回退，仅允许撤销本次rule6，不移除整条隔离策略。验收后两侧守护均已解除，回退材料保留；地址与MAC变化时须成对核对租约和ACL，不能直接放开整个VLAN3。

验证时有线重新接入，因而明确绑定Wi-Fi `en0`及其源地址：两台485网关及原`.0.236`设备均ping3/3，网关TCP8443空握手成功；未获管理授权的VLAN3测试源仍被拒绝。普通路由仍可优先使用有线，不能用该路径的成功代替Wi-Fi验收。HomeSense PID7249、异常重启0、网关发送队列0；未重启服务，也未发送总线控制。

VLAN100仍有网关等业务依赖，不能直接关闭三层接口、DHCP或标签。迁址、NAS与桌面设备接入按独立维护范围准备接口、地址与回退；依赖清空后才重新审核是否转为纯临时维护网。

临时维护参考[模板](../deployment/network/huawei-vlan100-maintenance.cli.example)，一次接一台缺省地址设备，核对地址冲突；临时地址池不发布网关、DNS或默认路由。结束后停用临时SSID／服务，移除本次新增且无其他依赖的标签，恢复PVID，不将临时开放状态保存为启动默认。

失败只恢复本次受影响设置。出现重复网关或环路先隔离新路径，再恢复已核对旧路径；不要整体恢复停用核心、旧聚合和临时互联。私有配置、配对、凭证、备份及失败日志保留。HomeSense现场验收独立见[接管结项](control4-takeover-checklist.md)。
