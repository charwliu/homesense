2026-10-15 变量反馈替代候选：独立从原XML重算25声明、224处显式读、263处写，487个事件/代码ID/操作/操作数/隐式读标记与既有全源审查一致。11个窗帘变量关联已审核的电机/LED事件，14个其他变量只关联灯光/场景/LED动作；按已确认反馈真值策略分别提出原生状态模型候选。其中3变量有14次算术隐式读，保留为“物理选择+待审执行状态”，不能简化成灯光开关值。未导入保存值、未解释type2编码、未删除旧变量或启用原程序；逐分支重写与生命周期处置仍是后续门槛。见[变量替代计划证据](evidence/stage-d-study/variable-feedback-replacement-plan-root-check.json)。

2026-10-15 场景关闭依赖边界补证：68个场景有51条可解析toggle引用，另外17个源值均为65535且全部成员目标为0%。原47处ACTIVATE_SCENE和48处DEACTIVATE_SCENE的有效目标全部存在；这17个场景没有被原程序再次关闭、翻转或用于场景条件，已解析的关闭调用使用相应叶场景的完整定义，不代表17个叶场景均被原程序调用。该结果不解释存档哨兵编码；“17个全关作为可重复执行按钮、其余保留完整关闭目标”的新语义建议已询问用户，尚未确认，未发布或启用。rate/跟踪编码与时序验收仍独立。见[关闭依赖边界证据](evidence/stage-d-study/scene-toggle-leaf-dependencies-root-check.json)。

# Control4 源语义与依赖审查

2026-10-15 面板事件声明已按原归档关联：GJ-TECH.CN_Keypad描述符版本21含12个明确事件，原28块面板的168条程序分别引用六个Release声明、每键28条；记录准确ID、原声明名称、描述符版本与指纹，不按偶数ID或按钮编号公式猜边沿。未找到描述符／事件时不补造，重复ID拒绝。三项新契约在旧工具中两错误／一失败保留，修正后连同17项已有用例共20项通过；完整权威源独立查表与差分确认，除新增声明关联／元数据外全部既有审查结果不变，四条日历创建者信息保留。只读取项目及指定明文描述符，未打开加密脚本／身份条目；只有两份Python审查文件改变，其他799份功能输入保持上一完整冻结回归，Rust／前端未改，本轮不重跑或累加1038项。原65／208／24全语义范围和现场门槛不减，没有新增批准、运行规则、发布或现场动作，README未改。见[面板声明根核查](evidence/stage-d-study/keypad-event-declaration-root-check.json)。

2026-10-15 原日历创建者来源关联已补齐：审查工具从每条entry/creatorid解析到同项目对象及描述符，四项分别对应驱动更新／组件更新代理各两项；另核对一份备份代理内嵌计划。四个事件的源空程序保持为空，但不据此抹去代理自身行为。缺失／悬空创建者明确未解析，重复创建者拒绝；不按描述文字猜所有者。原新增契约的两错误／一失败保留，修正后三项新回归及14项原回归共17项通过；权威完整源差分除新创建者信息外逐项一致，68／215／25和65／208／24范围不变。只有两份Python审查文件改变，Rust／前端／配置与上一完整冻结回归一致，本轮不重复全workspace或增加1038项计数；README未改。无新语义批准、任务、发布或现场动作，代理运行语义仍待审核。见[日历创建者核查](evidence/stage-d-study/calendar-creator-source-root-check.json)。

本页区分源结构、请求转换等价、接口契约和实物执行。语法成功、禁用候选、能力声明及限定人工证据均不等于完整程序通过。EA-5 配置和控制范围保持不变，审查不生成运行时定义。

2026-10-13 `0be525f` 的有序重复目标规则修复已独立软件核查：有持久规则审计的非空批次保留所有节点，按原顺序写出，立即 Level 节点不合并；外部重复意图、隐藏供电、人工覆盖、STOP 与事务门禁保持原约束。根的3份生产覆盖与提交字节一致，四项独立探针通过：同键 LED 双节点、六种原规划直接输出投影、纯变量规则及条件未命中规则；六子例共65帧，对应此前76向量的输出形态，省略原条件／变量／场景调用，不代表原完整程序重放。零命令规则仍走既有授权事务早返回，一条审计、零设备命令，未发现所担忧的空批次回归。另独立重算开发侧core94＋所选runtime300项、20结果块／19实际二进制及两包全部目标严格Clippy；首次并行96路失败与同源码串行通过均保留，原因未确定。不是完整workspace或当前Linux／现场验收，完整原语义与用户确认门槛继续挂起。见[规则顺序独立核查](evidence/stage-d-study/rule-order-root-check.json)。

2026-10-13 新增源规则运行时顺序缺口：已登记的7,652实际规划输出中，3个程序的76向量包含同目标的两个有序直接命令，61为LED、15为Level；源标签与顺序均保留，未批准原程序语义。根固定 `884850d` 的1,026原文件未改，新增有效schema2模拟面板探针，两个有来源的同键PanelLed(true)→PanelLed(false)规则节点在准入中被判重复目标，实际0通过／1失败、退出101，完成规则及命令日志均0，写门禁仍开，尚未创建TransportOwner。该合成次序验证一般顺序要求，不冒充原三程序完整重放；原规划另有Off→Off／Off→On及Level0→Level0。规则节点不得通过去重／省略来迁移，外部HAP／HTTP重复意图和隐藏供电直接控制门禁仍须保留。已交功能开发修复普通规则重复目标的有序输出；来源审计修复及原531／Linux7结果保持原范围，不被新反例自动否定为同一测试失败。见[规则顺序缺口根证据](evidence/stage-d-study/source-rule-order-gap-root-check.json)。

2026-10-13 `884850d` 的即时规则命令源节点审计缺口已独立软件验收：根固定提交1,026原文件未变，新增5项探针实际全通过，保留原反例并检查逐命令标签、延迟来源、第二条来源失败回滚及真实旧schema24／schema25备份兼容。组合灯另在同一项测试内覆盖前／后顺序和Switch Off／亮度0四种子例，10次命令出现的来源与持久intent均准确；四子例不计为四项测试。开发侧两包531项／72结果块／70实际二进制、15覆盖源码与最终提交及全部目标严格Clippy记录均独立重算核对，面板和混合延迟路径包含于该原531，不重复计数。新版本schema25不能由旧程序打开，历史来源保留NULL；这只关闭即时命令来源缺口，不批准原65场景／208程序／24变量或新版现场验收，也不称全工作区／Linux通过。见[最终来源审计根证据](evidence/stage-d-study/rule-source-audit-root-check.json)与[来源契约](rule-source-audit.md)。

2026-10-13 新增运行时源节点追踪反例：固定 `b31ddeb` 的独立副本原1,015文件未变，只新增一项合成探针。两个不同源标签的独立模块通道命令经实际 EventRules／PreparedRule／Admission 成功准入，重开 Store 后两条 Accepted 命令和规则关联仍在，但现有 rule_admission／rule_command／command_journal 没有任何节点标签；保留标签的断言实际0通过／1失败、退出101。静态链路在 from_snapshot 转为 Intent 时丢弃 source，当前审计只保留规则／事件／命令关联。最初重复设备批次被正确拒绝的夹具失败另保留，不算产品反例；全程未创建 TransportOwner 或网络服务。已交现有「功能开发」修复，独立复验待完成；延迟／渐变／场景展开边界仍需核对。此项不否定已有779项结果，但原完整回归未覆盖这个新增反例；完整原项目语义及现场门槛不变。见[运行时源标签缺口证据](evidence/stage-d-study/source-runtime-trace-gap-root-check.json)。

2026-10-13 同一固定提交的边界正例另实际通过1项：独立 schema2 的两通道延迟场景经 deferred_rule／SceneScheduler／Admission 执行合成接纳，重开 Store 后两条命令分别保留「调用节点 → 成员节点」完整来源。原1,015源码未改，只有新增探针；最初向调度器传入倒退时钟的夹具拒绝单独保留，修正后通过。既有完整779日志中的渐变专项也确实断言所有持久样本保留来源，本轮不重复执行或增加该旧数量。因此即时审计标签缺口不扩大为延迟成员／渐变来源全部丢失；即时及混合规则的即时命令修复仍待独立验收。见[延迟来源边界证据](evidence/stage-d-study/source-scene-trace-boundary-root-check.json)。

2026-10-13 运行时修复的初步限定候选另经根实际通过4项：原丢失反例保留且强化逐命令准确来源断言；原延迟来源边界保持；第二条来源记录插入失败时命令／变量／规则接纳全部回滚且关闭写门禁；直接使用旧 b31ddeb 反例留下的真实schema24数据升级，历史两条来源仍NULL、命令内容不变，schema24／25只读备份检查与25备份重开通过。候选仅7份生产覆盖文件，1,008原文件未变，4份实际二进制与日志／观测已保留；未以删除新列伪造旧格式。最终提交指纹、组合展开和面板／混合规则独立验收仍待完成，不据此关闭整个运行时来源门槛或原项目语义。见[初步候选根证据](evidence/stage-d-study/source-runtime-trace-candidate-root-check.json)。

## 已关闭的限定子项

| 子项 | 当前结论 | 仍保留的门槛 |
| --- | --- | --- |
| 20 个直线 LED 请求程序 | 独立解码源参数和顺序，与生产转换器的有序 AST 请求一致 | 均为 blind 代理回调；名义事件身份已有文档，原版本适用／通知来源、驱动行为、六键布局、绑定及所有权未验收 |
| 179 个可转换程序的归一化 AST 结构 | 独立 XML 模型逐事件核对字面量条件、AND／OR、嵌套 If／Else、变量表达式、场景引用和动作顺序，0 差异 | 含上述20个LED程序和4个源空程序；不是179个完整批准，源追踪标签、异步变量、触发／驱动／时序／物理执行另行审核 |
| 唯一 `RAMP_TO_LEVEL` 程序的显式渐变转换 | API参数单位为毫秒；固定生产接口的完整程序有序AST与独立XML模型一致，生成禁用定义保留TIME=1000／LEVEL=0 | 触发／异步变量／原驱动渐变实现／实物执行仍未审核；默认编译器的179／36计数不变，不用于推导其他rate字段 |
| 51 个场景的 Toggle 目标 | 源目标存在且唯一；48 条实际关闭调用全部有可解析目标 | 目标场景的完整时序、跟踪、绑定与执行仍待审核 |

LED 子集只允许启用的直线请求和明确的字面量键号；整数或字符串声明的规范数字选择器均保留源类型。未知代码字段、条件、禁用分支、动态值、未知容器属性或混合文本不获得该等价证据。结果仍为 `semantic_approved=false`、`runtime_rule_enabled=false`；这 20 个程序继续计入未完成全语义的范围。

官方 `SendToDevice` 示例把 `RAMP_TO_LEVEL` 的 `TIME=3000` 说明为 3 秒。这关闭命令参数单位问题，不证明驱动执行或存档字段的单位。[官方 API](https://snap-one.github.io/docs-driverworks-api/#sendtodevice)

该命令所属26代码项程序另通过独立完整结构核对：固定`1cd94a4`的实际`compile_event_with_fades`导出程序AST及一份禁用渐变定义，独立XML白名单模型解码条件／嵌套分支／变量表达式及动作顺序，RAMP扩展独立读取两个静态INTEGER参数，结果一致。错误单位、自动启用、错误目标、错误采样间隔、缺失定义及空动作六种变体均被拒绝。审计参数interval100ms／lateness100ms是显式转换选项，不冒充源字段；源TIME为1000ms、LEVEL为0。932原文件未变，1项生产导出测试通过，没有发布任何运行定义或发送命令；默认编译器仍拒绝RAMP，因此上项179归一化结构核对与36白名单外计数保持原口径。见[完整RAMP程序结构补证](evidence/stage-d-study/source-ramp-branch-differential-check.json)。

ALS 的 `DEACTIVATE_SCENE` 激活 Toggle ID 指向的目标，即使目标已活动；不等同取消运行。A/B 相互指向时只执行当前一跳的目标激活，不递归遍历 toggle 图。[官方 ALS 契约](https://snap-one.github.io/docs-driverworks-proxyprotocol-lightv2/#asynchronous-commands)

48 条源关闭调用的目标元素亮度均为零、成员和顺序一致，但 **9 条目标的 rate 序列不同于原场景**。只把原亮度改成零会遗漏目标定义差异；后续转换必须引用完整目标场景。另有 17 个未解析的源 toggle 值，保持未知，不解释成全部关闭或无操作。现有书房人工 `all_members_off` 策略仍属于明确批准的限定策略。

20 个 LED 程序的源事件分别属于代理 ID 3／5。官方 post-2.9 契约定义为通知到达后触发的 Stopped／Moving。这个名义接口身份已可核实；实际旧驱动何时通知、是否估计、版本适用与迁移触发来源仍未确认，不作为物理停止／移动证据。[官方事件定义](https://snap-one.github.io/docs-driverworks-proxyprotocol-blind/#blind-events)

## 已具体化的依赖门槛

官方自定义变量契约补证：通常跨正常重启保留已设置值，异常关机可能无法保留；项目加载事件可以显式初始化。不能将原变量全部映射成每次启动自动清零的临时状态。[官方变量处理](https://docs.control4.com/help/c4/software/cpro/dealer-composer-help/content/composerpro_userguide/variable_handling.htm) 数值变量示例另要求操作者明确输入0，它不证明所有变量的默认值为0。[官方数值变量示例](https://docs.control4.com/help/c4/software/cpro/dealer-composer-help/content/composerpro_userguide/example_using_a_custom_number.htm)

根重新核对权威归档及XML指纹：25个声明位于`currentstate/variables`，均为owner100001、type2、可写且非隐藏；215个事件中没有项目根节点owner1的事件程序。源项目声明版本为3.2.3／606460，不等于当前实机OS已验证；owner100001／type2的官方编码映射、逐变量生命周期和外部写入仍待审核。没有根事件不能证明所有隐式初始化均不存在；归档保存值仍不是批准的初始默认值，不据此删减整数比较域或批准24个剩余变量。此项只补文档接口与源事件边界，没有读取保存值或驱动脚本、生成运行定义或现场动作。见[变量生命周期接口根核查](evidence/stage-d-study/variable-lifecycle-interface-root-check.json)。

后续已关闭上段owner身份子项：[官方GetVariable接口](https://snap-one.github.io/docs-driverworks-api/#getvariable)明确用户变量代理DeviceID为100001，适用于源25个声明；[SetDeviceVariable](https://snap-one.github.io/docs-driverworks-api/#setdevicevariable)也允许驱动写该代理，因此XML显式写入不能证明外部写入集合完整。[AddVariable](https://snap-one.github.io/docs-driverworks-api/#addvariable)区分NUMBER、INT、FLOAT等名称，但没有给出存档type2的枚举映射，不能将它直接批准为整数或布尔。

审查工具现保留source_owner／readonly／hidden及有来源的名义代理／重启契约；原type_mapping／lifetime／initialization／external_writers门槛全部保留，不生成运行声明。14项既有工具回归通过，实际权威归档25声明归属逐项核对；去掉新增元数据后，68场景／215程序／25变量完整审查结果与旧工具完全一致，读224／写263不变。新增完整批准为0。见[变量代理身份与工具核查](evidence/stage-d-study/user-variable-owner-interface-root-check.json)。

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

`tools/compare_source_ast.py` 独立解码严格白名单源结构，与固定产品转换器导出的归一化AST比较，核对源XML与绑定输入指纹、215事件完整覆盖和观测类型。它不调用生产转换器来生成独立期望，不读取保存变量值、驱动Lua或身份库，不生成运行时配置。未知字段／容器、动态参数和不明Else归属不获得等价证据；不能因产品语法成功而提升独立模型未知项。私有输出拒绝覆盖并使用0600权限。

固定`1cd94a4`的932原文件未变，实际`compile_event`导出专项1项通过；独立模型边界6项通过。完整源215事件中179归一化动作树一致，36保留未知且产品也拒绝；对应275个If、1661个命令、215个变量赋值和95个场景引用。136个总线映射的地址／通道源属性另与既有候选表核对，仍只是源层级绑定。该核查不验证AST源追踪标签、变量生命周期、驱动实际渐变或物理反馈，也不改变65场景／208程序／24变量的完整语义剩余数。见[源结构差分证据](evidence/stage-d-study/source-branch-differential-check.json)。

2026-10-13源标签差分另在固定`bfe61f2`补齐：实际带标签AST与独立XML事件／代码／分支路径同时核对，179个默认可转换程序、2246节点（275 If／1661命令／215赋值／95场景引用）一致，含16个生成Toggle分支节点，同事件标签无重复。1项生产转换器导出通过，六种错误标签／覆盖变体被独立模型拒绝，949原文件未变；权威C4P哈希及其project.xml成员再次核对。它关闭的是源标签与结构对应证据，运行时TraceLabel消费者、源事件触发、异步变量和驱动实物行为仍未因此通过；36个原阻断保持未知。见[源标签根审计](evidence/stage-d-study/source-trace-root-check.json)。

源标签经过纯核心规划的保留另有直接证据：固定`bfe61f2`实际编译并规划179程序的7518组统一快照输入，独立模型从源树产生期望，全部保存的实际计划在动作／命令标签、变量、未知分支及反馈读集上相符，输入未改；1项实际Rust测试通过。42输入／程序含变量Unknown及-1／0..4、缺失／过期及0／17／30／100反馈，未使用原保存变量值。35650次命令和2242次未知分支标签属于规划结果出现次数，不是不同源站点或实际控制数量；统一向量不是全组合／全路径证明，持久准入、总线、触发和实物层仍待核对。见[源规划标签根审计](evidence/stage-d-study/source-planner-trace-root-check.json)。

2026-10-13 后续固定 `f7c2b22` 补齐条件路径证据：原 7518 组输入之外，49 组带来源的分支证据补齐 41 个条件节点的正／反例，84 组使用不同的双变量输入，并单独保留变量先减 100 后比较 3 的输入 103；实际 1 项集成测试的 7652 组规划差分通过。原 XML 的全部 215 事件独立重建、179 个可编译 AST 和实际输出逐份吻合，1008 原文件与提交及隔离副本一致。剩余两个条件节点各穷举 9 份三态反馈，仅在同一不可变快照下证明到达时必真，不证明 Control4 异步缓存更新路径不可达。这补充了上项均匀输入的不足，未批准 type2、变量生命周期、源驱动行为或完整程序；36 个默认阻断仍阻断。见[条件路径根核查](evidence/stage-d-study/source-branch-path-root-check.json)。

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
