# Control4 源语义与依赖审查

本页区分源结构、请求转换等价、接口契约和实物执行。语法成功、禁用候选、能力声明及限定人工证据均不等于完整程序通过。EA-5 配置和控制范围保持不变，审查不生成运行时定义。

## 已关闭的限定子项

| 子项 | 当前结论 | 仍保留的门槛 |
| --- | --- | --- |
| 20 个直线 LED 请求程序 | 独立解码源参数和顺序，与生产转换器的有序 AST 请求一致 | 均为 blind 代理回调；名义事件身份已有文档，原版本适用／通知来源、驱动行为、六键布局、绑定及所有权未验收 |
| 唯一 `RAMP_TO_LEVEL` 节点的 `TIME` | API 参数单位为毫秒，源静态参数已核对 | 完整分支、原驱动渐变实现、实物执行；不用于推导其他 rate 字段 |
| 51 个场景的 Toggle 目标 | 源目标存在且唯一；48 条实际关闭调用全部有可解析目标 | 目标场景的完整时序、跟踪、绑定与执行仍待审核 |

LED 子集只允许启用的直线请求和明确的字面量键号；整数或字符串声明的规范数字选择器均保留源类型。未知代码字段、条件、禁用分支、动态值、未知容器属性或混合文本不获得该等价证据。结果仍为 `semantic_approved=false`、`runtime_rule_enabled=false`；这 20 个程序继续计入未完成全语义的范围。

官方 `SendToDevice` 示例把 `RAMP_TO_LEVEL` 的 `TIME=3000` 说明为 3 秒。这关闭命令参数单位问题，不证明驱动执行或存档字段的单位。[官方 API](https://snap-one.github.io/docs-driverworks-api/#sendtodevice)

ALS 的 `DEACTIVATE_SCENE` 激活 Toggle ID 指向的目标，即使目标已活动；不等同取消运行。A/B 相互指向时只执行当前一跳的目标激活，不递归遍历 toggle 图。[官方 ALS 契约](https://snap-one.github.io/docs-driverworks-proxyprotocol-lightv2/#asynchronous-commands)

48 条源关闭调用的目标元素亮度均为零、成员和顺序一致，但 **9 条目标的 rate 序列不同于原场景**。只把原亮度改成零会遗漏目标定义差异；后续转换必须引用完整目标场景。另有 17 个未解析的源 toggle 值，保持未知，不解释成全部关闭或无操作。现有书房人工 `all_members_off` 策略仍属于明确批准的限定策略。

20 个 LED 程序的源事件分别属于代理 ID 3／5。官方 post-2.9 契约定义为通知到达后触发的 Stopped／Moving。这个名义接口身份已可核实；实际旧驱动何时通知、是否估计、版本适用与迁移触发来源仍未确认，不作为物理停止／移动证据。[官方事件定义](https://snap-one.github.io/docs-driverworks-proxyprotocol-blind/#blind-events)

## 已具体化的依赖门槛

629 个场景成员／元素的直接描述符均为 Light V2；按父协议包来源交叉核对如下。分类只陈述源文件关系，不推断实物能力。

| 父协议来源 | 成员数 | 原始 rate 字面量 |
| --- | ---: | --- |
| Switch 包 | 338 | 0：318；1：20 |
| Dimmer 包 | 291 | 1000：288；750：3 |

每个场景均有非零 rate，delay 全为零，成员 `ignore_ramp` 全为 False。不能挑出一个全零 rate 子集后忽略其他字段。两个未加密协议描述符都声明完整 ALS 支持、简化 ALS 关闭；实际脚本存在加密条目，未读取或执行。Dimmer 的 Ramp Timer 是枚举配置，公开选项没有单位依据；其默认值不能解释存档 rate。

Light V2 接口的 `SET_BRIGHTNESS_TARGET` 规定渐变参数使用毫秒，但完整 ALS 还有场景分发／通知契约。因此接口单位不能直接赋给存档 `AdvScene.rate`，也不能把完整 ALS 驱动假定为简化单目标命令执行。[官方 Light V2 文档](https://snap-one.github.io/docs-driverworks-proxyprotocol-lightv2/)

Blind 协议区分命令与 `MOVING`／`STOPPED` 通知，并要求驱动维护 Stopped 变量；级别范围可能因驱动而异。这提供接口类型依据，但没有证明项目 XML 的 `IF_MOVING`／`IF_STOPPED` 如何绑定到实际 HopeBeauty 驱动。代理报告、估计位置和物理观测需要分别记来源；收到开／关／停命令不能产生已确认物理运动状态。`LEVEL_TARGET_NAME` 中的符号值也不能直接改为 0–100% 位置。[官方 Blind 文档](https://snap-one.github.io/docs-driverworks-proxyprotocol-blind/)

变量清单完整保留 224 处显式读、263 处显式写，包括表达式、禁用来源及算术隐含读。3 个变量含算术写入，不能以二进制保存值代替整数程序审核；生命周期和初始化仍独立审核，保存值不导入初值。4 条日历入口关联源空程序，另外保留 1 个内嵌代理计划；源程序为空不代表代理自身行为已迁移，不生成家庭计划任务。

## 可复核流程

`tools/review_source_semantics.py` 只读取根 `project.xml` 和被场景引用的选定明文描述符；不读取身份数据库或 Lua 文件，不解密脚本。名称、保存变量值、具体家庭对象及指纹不进入公开摘要。新目录权限为 0700、文件为 0600，已有目录拒绝覆盖。

```sh
python3 tools/review_source_semantics.py SOURCE.c4p \
  --private-dir NEW_PRIVATE_DIRECTORY \
  --typed-review HASHED_TYPED_REVIEW.json \
  --prior-dispositions PRIOR_SOURCE_DISPOSITIONS.json \
  --prior-variables PRIOR_VARIABLE_DISPOSITIONS.json
```

可选 typed envelope 包含 `project_xml_sha256`、Rust `source_review::project_xml` 的 `review` 和 `direct_led_requests` 的 `direct_led_request_proofs`。源指纹与事件覆盖不匹配时拒绝合并；既有处置同样核验源指纹，只保留原有限定状态，不提升为完整审批。没有这些输入也可生成未审核清单。

当前完整逐项清单仅保存本地忽略目录。仍有 65 个场景、208 个程序和 24 个变量未完成原范围语义审核；本轮关闭的是明确的子项，D/E 未完成。38 项 importer 回归、所有目标严格 Clippy 及 14 项 Python 回归通过；没有重复运行已通过的 c184e65 全工作区测试。见 [聚合证据](evidence/stage-d-study/source-semantic-dependencies-check.json)。
