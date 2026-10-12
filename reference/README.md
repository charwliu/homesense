# 参考资料管理

仓库保留 HomeSense 自身的协议核对笔记、设备地址映射、Control4 解析脚本，以及来源和哈希清单。第三方产品说明书原文、提取全文、原页截图和 APP 下载图片不再随源码分发；编译、测试和运行不依赖这些资料副本。

2026-10-12 清理范围：

- `docs/产品手册/`：19 份 PDF 和 1 张 APP 下载图片。
- `reference/control4/manual-text/`：14 份提取全文。
- `reference/control4/manual-pages/`：7 张原页截图。

上述目录加入 `.gitignore`，本地如需查阅可自行恢复资料。`manifest.json` 的 `removed_third_party_documents` 保留原路径、来源信息及移除时 SHA-256；有原始复制记录的条目同时保留其复制哈希。这些条目是历史清单，不表示文件仍在仓库中。此次清理同时从本地与远端分支历史中移除上述目录；旧克隆需重新克隆或对齐重写后的历史，避免重新引入旧文件。

`Charlies250801.parsed.json` 和 `Charlies250801.report.txt` 是已忽略的私有项目快照，维持原有本地保存方式。`vendor/hap` 属于构建依赖，其源码、说明和许可证保持完整。
