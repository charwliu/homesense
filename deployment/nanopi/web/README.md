# HomeSense 双网 IPv4／IPv6 入口

2026-10-24已配置。域名仍为 `https://homesense.lab.vixal.xyz:8444`，两侧DNS的 `nanopi` 记录各含两个A、两个AAAA，`homesense`保留CNAME。App使用系统DNS和TLS，不将地址改为IP字面量。

`homesense-web-extra.socket.example`是脱敏模板，两个占位符须替换为当时确实分配给NanoPi的完整IPv6地址；不能直接安装含占位符的文件。生产文件位于 `/etc/systemd/system/`，`.service`模板与实际配置一致。

配套配置记录：[双侧DNS片段](homesense-dns.zone.example)、[VLAN2精确例外](vlan2-web-access.cli.example)。两者均使用IPv6占位符；DNS片段须合并到现有区域并递增SOA，核心例外须先核对现有规则及分类器，不能盲目重放。替换占位符后须按下述范围验证并保存，真实前缀仍仅保存在私有运维记录中。

- HomeSense主程序在 `.3.3:8444`独占运行；附加套接字为`.4.4:8444`及两侧IPv6的8444。透传不终止TLS，不复制私钥或凭证，也不增加第二个总线执行器。
- 三个附加入口共享32条代理连接上限，后端原有HTTP／WSS上限继续统一生效。
- 两个单元以主角色门禁和主服务为依赖；停止主服务时停止附加入口。socket启用链接在`homesense-e2.service.wants`，随主服务启动。显式延后网络套接字的默认启动顺序，避免依赖`network-online.target`时形成启动环。
- 当前前缀变化时，按既有暂停VLAN2 IPv6的保护流程，同步更新DNS、套接字和核心ACL后重新验证。显式绑定旧前缀不会自动追随新的PD。

核心精确例外：IPv4使用现有3980／3982的允许分类器，只匹配VLAN2与两个服务器IPv4之间的TCP8444请求／回包；IPv6使用3988／3989，匹配当前VLAN2的/64与两个服务器的/128、TCP8444，并在A3／B4两侧现有IPv6拒绝分类器之前放行。未开放SSH、HAP或其他设备。原始地址、下发与撤销命令、硬件success和保存结果均在私有运维证据中。

本次未替换或重启HomeSense主程序。配置前保存原单元及主进程指纹，设置独立120秒恢复守护；安装后在四个实际地址分别完成HTTPS诊断读取、WSS升级和域名校验，再解除守护。主进程PID、主程序SHA和冷备归属保持。两侧DNS分别设置独立回退后依次更新，区域检查、UDP／TCP、外部DNS与NGR原记录通过。

用户终端最终验收：连接NGBOSS（VLAN2），以`https://homesense.lab.vixal.xyz:8444`重新连接后成功显示状态。此结果验证域名入口可用，不声称终端已强制逐个验证四条地址路径；验收未执行灯光、窗帘或风机动作。

撤销附加入口时，先停止并禁用`homesense-web-extra.socket`、停止对应`.service`，再移除这两个配置并重载systemd；保留HomeSense主服务和`.3.3`入口。仅在DNS不再公布附加地址、核心例外已按本次备份核对后清理相应记录与规则；不删除其他管理例外。
