# Control4 源语义与依赖审核

导入工具区分源结构、转换等价、权限与实物执行。语法成功、禁用草稿或能力声明不等于完整程序获批；控制范围和启用由明确审核计划决定。

## 审核内容

- 固定归档与展开 XML 指纹，关联准确事件声明、描述符版本、代码节点和容器路径，不按编号公式猜按下／释放。
- 保留原条件、Else、变量表达式、节点顺序、重复目标和嵌套场景；不能去重有序命令来消除转换障碍。
- 显式变量读写及算术隐含读分别记录；保存变量值不直接作为运行初值，复合整数不能仅凭数值当作位掩码。
- 场景激活／停用保留准确目标配方，不把停用改成来源成员全关或反转。
- 原始运动通知、驱动报告与时间估计分开；Open／Close／Stop 不构成已确认的运动或位置反馈。
- 日历创建者、代理内嵌计划及源空程序分别审查，源程序为空不表示代理行为已经迁移。

运行时命令保存准确来源标签，供命令、规则、变量和场景审计关联。详见[规则节点审计](rule-source-audit.md)、[停用配方](reviewed-deactivation.md)、[窗帘导入](motor-rule-import.md)。

## 独立核对

`tools/compare_source_ast.py` 从白名单源结构独立产生归一化模型，与产品转换器输出比较。它不调用产品转换器生成自己的期望，也不读取保存变量、驱动 Lua 或身份库；未知节点保持未支持，不能因产品可解析自动提升为等价。

`tools/review_source_semantics.py` 读取根 `project.xml` 与指定明文描述符，生成私有审核报告：

```sh
python3 tools/review_source_semantics.py SOURCE.c4p   --private-dir NEW_PRIVATE_DIRECTORY   --typed-review HASHED_TYPED_REVIEW.json   --prior-dispositions PRIOR_SOURCE_DISPOSITIONS.json   --prior-variables PRIOR_VARIABLE_DISPOSITIONS.json
```

可选 typed envelope 绑定 `project_xml_sha256`、`source_review::project_xml` 的 `review` 及 `direct_led_requests` 证明。指纹、事件覆盖或先前处置不匹配时拒绝合并；没有审核输入可生成未审核清单，但不会自动批准或启用。

新输出目录拒绝覆盖，目录权限0700、文件0600。家庭对象、来源指纹和完整配置留在私有目录。导入审核不能替代实际接管验收，已交付范围见[结项清单](control4-takeover-checklist.md)；阶段审核过程见[历史档案](archive/de-completion-audit.md)。
