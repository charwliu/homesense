# NanoPi → NGR 定时数据备份

NanoPi 独占运行 HomeSense；NGR仅通过SSH接收数据快照，`homesense-e2.service`保持disabled、MainPID=0，root主角色门禁关闭。备份程序没有启动、停止或启用HomeSense的操作。

## 时间、范围和保留

- 每小时05分附近备份一次，随机错开最多60秒；开机后5分钟补做，持久定时器补做错过的任务。
- NGR保留最近24份快照，另保留最近7个有备份日期的每日最新一份；两组重叠只保留一次，最多31份。每日边界按Asia/Shanghai计算。
- 覆盖当前六个运行SQLite数据库、控制器ID、设备／自动化／HAP配置、启动凭证和TLS证书链／私钥，共13个运行文件。
- 不备份ELF、源码、测试夹具、旧构建目录、抓包或历史调试报告。NGR现有两份生产ELF独立保留。
- HAP 当前使用 `study-e` 单桥身份；备份白名单按实际运行状态布局校验，可能保留退役桥的数据以供恢复，不据备份文件数量判断活动桥数。变更运行数据库或状态布局前，先验证白名单。

使用SQLite在线Backup API读取已提交WAL，不复制仍在写入的数据库文件，也不暂停主服务。每个数据库生成独立的一致性快照。备份前、快照中及备份后核对活动配置、HomeKit身份／配对／AID／IID和App权限指纹；该窗口若发生关键变更，本次任务失败并保留日志，不把不匹配的组合标记为成功。导出的设备和自动化JSON来自备份数据库的活动版本，避免仅复制过期的启动JSON。

这不是六个数据库的全局原子事务，也不是实时副本；恢复点以每份成功快照为准。

## 实际位置

| 主机 | 位置／用途 |
| --- | --- |
| NanoPi | `homesense-backup.service`／`homesense-backup.timer` |
| NanoPi | `/etc/homesense-backup.json`：root管理的传输配置 |
| NanoPi | `/home/alarm/apps/homesense/backup/`：专用密钥、固定主机公钥、成功回执和失败日志 |
| NGR | `/etc/homesense-backup-receiver.json`：root管理的接收目录配置 |
| NGR | `/home/build/apps/homesense/backups/nanopi-periodic/snapshots/`：已验证快照 |
| NGR | `/home/build/apps/homesense/backups/nanopi-periodic/latest`：最新成功快照的原子符号链接 |
| NGR | 同目录 `last-success.json` 和 `failures/`：接收回执和失败日志 |

每份快照含 `manifest.json`，记录创建时间、配置revision、主程序SHA、文件大小与SHA-256及关键身份指纹。目录权限700，数据文件600。生产运维工具须匹配各自主机的 Python 版本；测试和产品源码不随部署上传。

专用SSH私钥保留在NanoPi；NGR公钥授权限制来源IPv4、强制备份接收命令，并禁止shell、PTY与转发。接收器以不可提权方式执行，传输严格校验固定的NGR主机公钥。此密钥没有恢复数据或启动服务的接口。

NGR接收前及提交前都核对冷备状态。流内只接受13个固定名称的普通文件，拒绝路径穿越、链接、重复文件、越限大小及SHA不符；数据库与关键指纹验证完成后，才原子更新latest并清理过期成功快照。失败保留日志，旧latest继续可用。任务之间加锁，不并发覆盖同一个工作目录。

## 查看和手动备份

在NanoPi查看定时器与最近运行：

```sh
systemctl list-timers homesense-backup.timer
systemctl status homesense-backup.service
journalctl -u homesense-backup.service --since today
cat /home/alarm/apps/homesense/backup/last-success.json
```

需要额外备份时，在NanoPi执行：

```sh
sudo systemctl start homesense-backup.service
```

只需暂停自动备份时，停止 `homesense-backup.timer`；不要为此操作HomeSense主服务。任务失败时检查journal和两端failures目录，修复具体原因后再验证；定时器下次正常调度不清除已有失败日志。

## 恢复边界

接收快照不会覆盖NGR原生产状态、运行配置或角色门禁，也不会自动接管。恢复前遵循[主备恢复顺序](../README.md#手动恢复边界)：先可靠隔离／停止旧主，再选择成功快照、备份目标原状态、转换HAP监听主机地址并核对身份、配置同域名DNS，最后开启唯一主角色并只读验收。

快照文件与NGR生产路径的对应关系：

| 快照 | 恢复目标 |
| --- | --- |
| `state/` | NGR的生产状态目录；保留整个六库组合及web-controller-id |
| `config/devices.json`、`automation.json` | 当前活动设备／自动化启动文件，按对应revision配置单元 |
| `config/hap-plan.json` | NGR的HAP计划；与实际活动 identity 库的 host 一起从 NanoPi 地址迁回 NGR 地址 |
| `secrets/bootstrap-token` | NGR的 `service/secrets/e2-bootstrap-token` |
| `tls/server-chain.pem`、`server-private.pem` | NGR服务单元实际引用的同域名证书／私钥 |

备份记录了源程序SHA和数据库schema；将来软件或schema升级后，恢复前须核对NGR的x86_64 Release兼容性。不能在NGR运行ARM ELF，也不能只回退一个数据库。每份快照都含凭证、配对和TLS私钥，不放入Git或公开下载位置。
