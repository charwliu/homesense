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
| X1，QNAP A | Trunk | Tagged2／3／100，PVID4093 | 不适用 |
| X2，QNAP B | Trunk | Tagged2／4／100，PVID4094 | 不适用 |
| 2，AC100 | Access | 5 | 关闭 |
| 5，R6S wan | Access | 3 | 关闭 |
| 6，R6S lan1／br2 | Access | 4 | 关闭 |
| 15–22，AP | Trunk | Tagged2／3／4／100，管理PVID5 | 按实际供电 |

现用485网关经QNAP A的8号Untagged100接入，核心10号是预留口。Trunk隔离PVID不提供业务三层服务；VLAN4092维护入口单独保留。

### QNAP

QSS中T表示Tagged、U表示Untagged，逐VLAN核对成员及PVID。同一Combo光／电侧不能同时占用，10号Combo是核心上联。NAS沿已登记路径使用，X3／X4使用前重新核对；不能按规划图直接迁线。

每次单项保存后读回成员、PVID、链路与错误计数，再验证本地维护入口。未经实机核对，不移动QSS管理CPU到另一VLAN。

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

VLAN3通过DHCP Option121取得`192.168.0.0/24 → 192.168.3.253`、AC专用路由和原默认出口。VLAN100仅允许指定管理终端及已审核服务例外；固定租约与真实源地址不公开。VLAN2允许IPv4访问AC`.5.1/32`。

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

独立R6S的两张网卡使用`.3.3`和`.4.4`，不桥接两网。域名`homesense.lab.vixal.xyz`指向`nanopi.lab.vixal.xyz`，两侧DNS发布对应A／AAAA；HTTPS／WSS在TCP8444提供同一服务。

前缀变化同步维护[附加监听](../deployment/nanopi/web/README.md)及精确回包规则。双网接收与发送可能走不同接口，核对真实流量再调整，不能放宽到无关内网。HAP使用单座原身份桥，不能通过同时启动NGR来测试可达性。

NGR只接收[定时数据备份](../deployment/nanopi/backup/README.md)，服务禁用、主角色关闭。恢复须先停止或可靠隔离旧主，再按[NanoPi恢复流程](../deployment/nanopi/README.md#手动恢复边界)处理。

## VLAN100维护与恢复

VLAN100仍有网关等业务依赖，不能直接关闭三层接口、DHCP或标签。迁址、NAS与桌面设备接入按独立维护范围准备接口、地址与回退；依赖清空后才重新审核是否转为纯临时维护网。

临时维护参考[模板](../deployment/network/huawei-vlan100-maintenance.cli.example)，一次接一台缺省地址设备，核对地址冲突；临时地址池不发布网关、DNS或默认路由。结束后停用临时SSID／服务，移除本次新增且无其他依赖的标签，恢复PVID，不将临时开放状态保存为启动默认。

失败只恢复本次受影响设置。出现重复网关或环路先隔离新路径，再恢复已核对旧路径；不要整体恢复停用核心、旧聚合和临时互联。私有配置、配对、凭证、备份及失败日志保留。HomeSense现场验收独立见[接管结项](control4-takeover-checklist.md)。
