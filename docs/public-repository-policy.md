# 公开仓库内容与隐私检查

公开内容包括产品源码、合成回归夹具、脱敏配置示例、协议说明及功能／使用文档。现场设备与自动化配置、工程解析快照、凭证、签名材料、数据库、抓包、截图和原始验收日志保存在本机已忽略目录，不随源码分发。

## 保存与生成

- 现场配置保留在本地 `config/live/`、`config/import/`、`config/scenes/` 或独立 `private/` 目录，不能作为公开示例复制。
- `docs/evidence/` 和部署目录下的 `evidence/`、日期批次JSON属于本机验收资料。公开文档可以保留限定结论，但不链接私有文件下载。
- 自动化测试使用回环／内存网关与合成身份。`config/test/legacy-study/` 为兼容性测试保留数值标识，端点、名称、指纹和验证说明均为合成值，不授权现场控制。
- 早期限定调试工具 `study-scene` 显式读取本地文件，不内嵌现场配置：

```text
study-scene DEVICES_JSON SCENES_JSON STATE_DIR KEY(1|3|5) --coexistence-confirmed [activate|toggle|cancel-before|cancel-after-one]
```

该入口仍按自身限定映射、型号和权限校验；输入文件须由操作者独立审核。日常控制与部署按主指南使用统一服务。

## 提交前检查

```sh
python3 tools/test_check_public_tree.py
python3 tools/check_public_tree.py
```

检查器读取 Git 索引中的真实内容，报告私有路径、常见凭证格式和网络模板中的非文档IPv6地址；只输出位置与类别，不输出匹配值。索引清理后重新扫描，未跟踪的本地文件不会因检查自动上传。

模式检查不能证明任意文本绝不含秘密；提交前仍需人工核对文件范围、合成数据来源及脱敏内容。私有记录从索引移除时保留本机原文件，核对哈希；公共历史清理另外备份并核对远端引用，不能将本地私有备份分支一起推送。
