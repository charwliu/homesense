# 组件确认与只读场景活动

`scene_component_activity` 将即时场景的活动显示关联到已有组件确认策略。它不建立选择型开关，也不改变场景的执行动作。

```json
{
  "scene_component_activity": [{
    "scene": "reading",
    "enabled": true,
    "variable": "reading_mode",
    "active_value": true,
    "activity_name": "阅读",
    "review_evidence": "Reviewed component and scene association"
  }]
}
```

配置必须指向已声明的持久整数或布尔变量，类型与 `active_value` 相同；同一场景及活动值必须已有经过审核的 `physical` 组件确认策略。场景不可同时声明另一个跟踪、选择或呈现提供者。名称或灯光组合相同不会自动建立关联。

活动状态需要同时满足：

- 该启动／配置／传输代次中仍有效的组件确认所有权；
- 当前变量版本和值仍属于该确认记录；
- 对原物理向量重新读取的新鲜、已稳定反馈，以及反馈租约重验。

确认中的操作显示 `Pending`。确认记录的值等于活动值时为 `Active`，其他已确认值为 `Inactive`。反馈矛盾、过期、变量版本变化、重启或没有物理确认记录时为 `Unknown`。只有 `logical_source` 的赋值不能独立变成已确认活动；这类后续路径仍需另行补齐执行和物理状态证明。

读取目录不会发送查询、执行场景、提交确认或修改变量。接口保持 `activity_kind: read_only`、`switch_kind: momentary` 和 `selected: null`；`activity_source` 为 `confirmed_component`。原生卡片可使用活动状态显示底色，点击仍调用原来的执行入口。

此能力已接入软件候选；私有配置、Linux 整合资格、部署及现场验收分别记录，不以本说明代替上线证明。
