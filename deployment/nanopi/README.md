# NanoPi HomeSense 主服务器

2026-10-24：NanoPi R6S 已接替 NGR 运行 `homesense-e2.service`，活动配置仍为 revision 52。NGR保留为手动冷备，每小时接收一次在线数据备份；未配置自动故障切换。详见[定时备份](backup/README.md)。

## 当前入口与接线

| 项目 | 当前配置 |
| --- | --- |
| App／HTTPS／WSS 入口 | `https://homesense.lab.vixal.xyz:8444` |
| DNS | `homesense.lab.vixal.xyz` CNAME → `nanopi.lab.vixal.xyz`，A 为 `192.168.3.3` |
| VLAN3 | 核心5号 Access3 → R6S 的 wan；`192.168.3.3/24`，RA 固定后缀 `::3:3` |
| VLAN4 | 核心6号 Access4 → lan1／br2；`192.168.4.4/24`，RA 固定后缀 `::4:4` |
| 链路 | 两口均已核对为2.5Gbps，PoE关闭 |
| 启动系统 | 当前microSD系统；未更换到eMMC |
| 首选IPv4出口 | `192.168.4.253`，br2，源地址 `.4.4`；保留A线备用默认路由 |
| 485网关 | 仍为 `192.168.0.10:8443`，经VLAN4访问 |
| HAP桥 | 沿用原身份和配对，监听 `.3.3:51828` 与 `.3.3:51829` |
| NGR冷备 | 保留 `.3.2/.4.2`，服务disabled、无运行进程，主角色门禁关闭 |

独立R6S不是核心23号口的NanoPi5路由器。两张服务器网卡不桥接VLAN3和VLAN4，不提供DHCP、RA或跨VLAN转发。DNS的AAAA使用当前VLAN3前缀；前缀变化后须同步维护静态记录。当前HomeSense Web监听显式IPv4地址，域名AAAA登记不代表HTTPS已有原生IPv6监听。

## 程序、证书与状态

生产根目录为 `/home/alarm/apps/homesense`：

- `releases/20261024-primary-0c3896f7c184/homesensed`：ARM64 Cargo Release，35,539,632字节，SHA-256 `0c3896f7c184b47b148ef8696fe90708671e1556fd7195dd7dd2371eb7e58ba1`。
- `service/config/`：设备、自动化及HAP计划；本轮仅迁移HAP监听地址，未发布新配置版本。
- `service/state/`：从停止后的NGR最终状态复制；保留两个桥的身份、配对、AID／IID以及App凭证与权限。
- `service/tls/`：用户提供的 `homesense.lab.vixal.xyz.pem` 和 `homesense.lab.vixal.xyz-key.pem`；证书含服务器叶证书和中间CA，私钥权限600。
- `service/secrets/`：已有启动凭证；不在公开文档记录其内容。
- `/var/lib/homesense-role/primary`：root管理的主角色门禁；缺失时服务不能启动。

客户端使用域名入口，并信任该证书的签发CA；此前旧HomeSense CA的信任不能自动视为对新CA的信任。证书SAN不含IP地址，不通过IP进行HTTPS域名验证。

## iPad／iPhone 连接

两台设备分别连接NGR无线（VLAN3），安装该域名证书对应的公开根CA，并在“设置 → 通用 → 关于本机 → 证书信任设置”启用其完全信任。只向设备提供公开根证书，不分发服务器私钥。手动安装后的信任操作见[Apple说明](https://support.apple.com/zh-cn/102390)。

在HomeSense右上角菜单进入“服务连接”，先“断开服务”，将“HTTPS 地址”设为 `https://homesense.lab.vixal.xyz:8444`，输入原“访问令牌”，点击“连接 HomeSense”。不必移除旧凭证或重新配对Apple“家庭”。

App按含端口的HTTPS地址将凭证保存在每台设备本地Keychain，不通过iCloud同步；更换域名后通常需要在两台设备分别输入一次原令牌。迁移保留了服务端原凭证和权限，不因改地址重新签发管理员凭证。初始化令牌位于服务器 `service/secrets/e2-bootstrap-token`；独立签发的App令牌应使用其原私有交付文件，不能把服务端权限数据库当作可复制的明文令牌。

## 本次验收范围

本机冻结源码对应现网NGR版本，已有1863项对应源码的本机回归及严格Clippy资格记录；本轮复用根target，以Cargo交叉编译ARM Release。目标只上传生产ELF及明确授权的运行配置、身份数据和证书，未上传源码、测试ELF或夹具。

切换前在NGR完整冷备状态树，六个活动SQLite数据库完整性检查通过；最终运行文件经加密SSH流式复制到R6S，不在Mac落盘保存生产凭证／配对快照。两台主机分别设置独立停止／恢复守护；NGR停服、禁用并关闭主角色门禁后才启动R6S。

新主机连续三轮只读验收：rev52、12模块96路confirmed反馈、队列清空、NRestarts=0，启动审计无验收控制命令。两个桥的不可变身份、配对、AID／IID与App权限指纹一致；现场已有HomeKit控制器成功认证并读取状态。Mac系统信任库验证域名TLS成功，无凭证访问返回401；原凭证的WSS升级与只读订阅成功。未执行灯光、窗帘或风机动作验收。

NGR只保留当前与最近回退两份x86_64生产ELF。冷备的同域名TLS和systemd配置已校验，未为验证而同时启动第二个HAP／总线执行器。

## 手动恢复边界

NGR原生产目录保留切换时的完整状态，后续每小时备份存入隔离的nanopi-periodic目录，不自动覆盖旧生产状态或启动备机。恢复前须明确选用最新成功快照并按版本恢复；冷备不是实时副本。

恢复操作顺序：

1. 先停止并禁用R6S服务、关闭其主角色门禁；若R6S失联，须确认已断电或隔离其总线连接，不能仅凭ping失败启动NGR。
2. 在目标仍停止的状态下，备份原状态并复制选定的完整运行数据库、控制器ID、凭证和活动配置；仅把HAP保存配置与计划的主机地址迁回 `.3.2`，核对不可变身份和配对指纹不变。
3. 更新 `homesense.lab.vixal.xyz` 指向NGR实际HTTPS监听地址 `192.168.3.2`，保持同域名证书。不能未经核对就使用含多个地址的 `ngr` DNS记录，也不能让同一名称同时保留CNAME和A。
4. 为NGR打开root主角色门禁，启用并启动服务，进行域名TLS、凭证、HAP和96路反馈的只读验收；设置独立失败回退后才执行。
5. 回切R6S遵循同样的先停旧主、复制最终状态、再开新主顺序。任何时刻只允许一个执行器与同一组HAP身份运行。

完整备份、SHA、证书检查、状态指纹与切换日志保存在忽略的私有运维目录；公开仓库不保存实际全球IPv6前缀、配对数据库或凭证。
