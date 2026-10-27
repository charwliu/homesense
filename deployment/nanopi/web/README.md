# HomeSense 双网 IPv4／IPv6 入口

域名为 `https://homesense.lab.vixal.xyz:8444`，两侧DNS的 `nanopi` 记录各含两个A、两个AAAA，`homesense`保留CNAME。App使用系统DNS和TLS，不将地址改为IP字面量。

`homesense-web-extra.socket.example`是脱敏模板，两个占位符须替换为当时确实分配给NanoPi的完整IPv6地址；不能直接安装含占位符的文件。生产文件位于 `/etc/systemd/system/`，`.service`模板与实际配置一致。

配套配置记录：[双侧DNS片段](homesense-dns.zone.example)、[VLAN2精确例外](vlan2-web-access.cli.example)。两者均使用IPv6占位符；DNS片段须合并到现有区域并递增SOA，核心例外须先核对现有规则及分类器，不能盲目重放。替换占位符后须按下述范围验证并保存，真实前缀仍仅保存在私有运维记录中。

- HomeSense主程序在 `.3.3:8444`独占运行；附加套接字为`.4.4:8444`及两侧IPv6的8444。透传不终止TLS，不复制私钥或凭证，也不增加第二个总线执行器。
- 三个附加入口共享32条代理连接上限，后端原有HTTP／WSS上限继续统一生效。
- 两个单元以主角色门禁和主服务为依赖；停止主服务时停止附加入口。socket启用链接在`homesense-e2.service.wants`，随主服务启动。显式延后网络套接字的默认启动顺序，避免依赖`network-online.target`时形成启动环。
- 当前前缀变化时，按既有暂停VLAN2 IPv6的保护流程，同步更新DNS、套接字和核心ACL后重新验证。显式绑定旧前缀不会自动追随新的PD。

双网主机对 VLAN4 IPv6 地址的回复必须经 VLAN4 的 NanoPC 返回。若按目的地址直接从 VLAN3 网口回复，同一连接的 SYN 会经过 NanoPC、SYN-ACK 却绕开它，后续 ACK 可能被其连接跟踪判为无效并丢弃，表现为 TCP 已连接但 TLS 超时。

[`60-homesense-ipv6-return.conf.example`](60-homesense-ipv6-return.conf.example)为源地址回程模板：专用表204保留 VLAN4 本地前缀直连路由，其他目的经 NanoPC 的 VLAN4 链路本地地址；优先级10404仅匹配 HomeSense 当前 VLAN4 IPv6 的/128。它不改变 VLAN3 地址的回程、不扩大核心放行范围，也不重启HomeSense。安装前须确认表和优先级未占用并设置独立回退；通过`networkctl reload`／`networkctl reconfigure br2`应用后，核对 PID和网关连接未变，并分别从 VLAN3有线、VLAN2 Wi‑Fi 验证四个HTTPS入口。前缀变化时需与DNS、监听地址同步更新该From地址和直连前缀；当前模板不宣称自动跟随PD。

核心精确例外：IPv4使用现有3980／3982的允许分类器，只匹配VLAN2与两个服务器IPv4之间的TCP8444请求／回包；IPv6使用3988／3989，匹配当前VLAN2的/64与两个服务器的/128、TCP8444，并在A3／B4两侧现有IPv6拒绝分类器之前放行。未开放SSH、HAP或其他设备。原始地址、下发与撤销命令、硬件状态和保存结果留在私有运维记录。

## 安装与核对

变更前保存原单元与 DNS／ACL，设置独立恢复守护。替换占位符后，在四个地址分别核对域名证书、HTTPS 诊断和 WSS 升级，并检查两侧 DNS 的 UDP／TCP 结果；通过后解除守护。服务入口验证不发送设备控制。

撤销附加入口时，先停止并禁用`homesense-web-extra.socket`、停止对应`.service`，再移除这两个配置并重载systemd；保留HomeSense主服务和`.3.3`入口。仅在DNS不再公布附加地址、核心例外已按本次备份核对后清理相应记录与规则；不删除其他管理例外。
