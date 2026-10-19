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

确认中的操作显示 `Pending`。确认记录的值等于活动值时为 `Active`，其他已确认值为 `Inactive`。反馈矛盾、过期、变量版本变化、重启或没有物理确认记录时为 `Unknown`。只有 `logical_source` 的赋值不能独立变成已确认活动。

读取目录不会发送查询、执行场景、提交确认或修改变量。接口保持 `activity_kind: read_only`、`switch_kind: momentary` 和 `selected: null`；`activity_source` 为 `confirmed_component`。原生卡片可使用活动状态显示底色，点击仍调用原来的执行入口。

此能力已接入软件候选；私有配置、Linux 整合资格、部署及现场验收分别记录，不以本说明代替上线证明。

## 已知程序的关闭与再次开启

已知分支可以保留原来的即时变量赋值，并通过可选 `program_confirmation` 单独证明只读活动。它不会将该赋值转换成物理确认，也不会推迟、撤销或重写原控制程序。

```json
{
  "program_confirmation": {
    "active_conditions": [{"device": "reading_light", "on": true, "level": 10}],
    "known_values": [false, true],
    "timeout_ms": 15000,
    "max_age_ms": 5000,
    "review_evidence": "Reviewed program outcomes and complete shared component"
  }
}
```

该字段位于每条 `scene_component_activity` 关联内。`active_conditions` 必须恰好包含该场景所有匹配物理确认向量的共同开启条件，不能只取其中一部分；`known_values` 与原持久变量类型一致，包含活动值并明确列出允许证明的其他结果。

运行时记录实际执行分支的变量读集、提交版本、权限来源和最终灯光目标。全部灯光命令实际写出，且完整目标获得写后真实反馈，才建立活动事实。等待反馈使用原控制期限内最多15秒的预算；已成功写出的发送票据时限不会缩短反馈窗口。确认后的活动只在原观察新鲜度内有效，读取目录不会续期。

未操作的独立组件必须继承仍有效的组件所有权及对应物理条件，不能借一条无关灯光命令、旧变量或相同光型补造身份。已确认组件保持原实际签名；外部变化、同值新版本、权限撤销、代次变化或重启均使旧证明失效，之后的匹配反馈不会自行恢复它。

这种已知程序证明返回 `activity_source: confirmed_program`，仍保持 `read_only`、`momentary` 和 `selected: null`。未配置此选项的关联继续使用原物理组件提供者；提供者缺失时仍为未知。该能力不增加查询或控制命令。
