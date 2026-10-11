# ADR 0001：原生 Rust HAP 候选核查

日期：2026-10-10；状态更新：2026-10-11。受控候选已通过 A3 指定的真机配对／读写／反馈及重启身份验证，a21 已完成书房主灯现场序列。全屋及长期验收尚未完成。以下保留初始发现与修复历史；最新要求证据见 ../evidence/stages-a-c-requirements-20261011.md。

## 固定来源与复现

上游为 [ewilken/hap-rs](https://github.com/ewilken/hap-rs/tree/acf45bdb00e23c81b67b3d5116c113335dc83f16)，提交 `acf45bdb00e23c81b67b3d5116c113335dc83f16`，包版本 `0.1.0-pre.15`。测试工具链是本机 Rust 1.99.0，aarch64-apple-darwin。

`probes/hap-rs/` 保存固定依赖锁文件、最小编译补丁、受控候选补丁、回调行为探针和复现脚本。受控源码现已置于 `vendor/hap` 并由 `homesense-hap` 使用；原始上游仍在主 workspace 之外。脚本在临时目录检出固定提交，退出后删除临时 checkout；编译产物位于 `target/hap-candidate`。不会启动示例、生成配对身份或广播配件。

```sh
# 已有本地上游 checkout 时无需访问 GitHub；依赖需先缓存。
bash probes/hap-rs/check.sh /private/tmp/homesense-hap-rs-review
# 不带参数时从上游克隆；仍按保存的锁文件离线构建。
bash probes/hap-rs/check.sh
```

上游原样构建失败：`get_if_addrs-sys` 和 `if-addrs-sys` 同时声明 `links=ifaddrs`。最小补丁把 `get_if_addrs` 重命名依赖指向 `if-addrs 0.6`，与 libmdns 使用同一包。补丁后 lightbulb 示例编译通过，上游仍有弃用和生命周期警告，未声称通过主工程的 Clippy 门槛。

## 特征接口发现

源码位置 `src/characteristic/mod.rs` 和 `src/transport/http/handler/characteristics.rs`：

- `get_value` 的读回调返回值经过 `set_value`，会触发写回调。直接把读回调连接到 observed、写回调连接到总线会导致读请求产生控制意图，必须拆开。
- 写回调成功后立即更新缓存并可发送事件。不能把“准入成功”当成负载 confirmed，必须提供独立的观测更新路径及状态读取。
- 写回调失败保留原缓存；HTTP 层的错误映射仍需按 HomeSense 拒绝原因验证。
- 亮度元数据声明 0..100，但底层设置值没有执行这些边界，适配层必须校验。
- 单次 HTTP 请求中的多个特征逐个处理，现有回调不携带整组 On/Brightness 或控制器身份。需要请求级适配接口，不能靠回调顺序拼出原子意图。

最初四项离线探针记录未修复候选的行为；后续补丁与回归结果见下节，不能把这些初始发现当成当前补丁的行为。

## 持久化与依赖待修复项

上游 FileStorage 直接截断写文件，且配置加载失败的示例路径会重新生成身份。HomeSense 已实现独立 IdentityStore：受限目录、SQLite 原子持久化、错误时停止加载而非重置、身份／AID 缓存；不把 PIN、私钥或完整配置写入日志。服务启动与稳定设备 ID 到 AID 的映射仍待接入。

初次构建锁文件包含 ed25519-dalek 1.0.1、curve25519-dalek 2.1.3 和 3.2.1。已核对 [RUSTSEC-2022-0093](https://rustsec.org/advisories/RUSTSEC-2022-0093.html) 与 [RUSTSEC-2024-0344](https://rustsec.org/advisories/RUSTSEC-2024-0344.html)；版本位于公告影响范围，尚未完成本调用路径可利用性分析或全依赖审计。修复编译冲突不代表修复这些依赖问题。后续升级应使用现有加密库 API，并验证 HAP 配对向量，不自制加密原语。

特征写入 HTTP body 已增加 64 KiB 累计上限、5 秒读取超时和 1–128 项特征限制；请求体接收后重新读取配对记录，未认证或已删除配对拒绝写入。握手／其他端点限制、会话密钥与配对公钥变更、撤销竞争及事件背压仍需核查和回归。当前没有启动局域网服务，没有连接真实总线，没有 iPhone 配对证据。

## 受控补丁进展

`candidate.patch` 相对于固定上游提交包含编译兼容及 API 修复，`check.sh` 已应用此补丁；`if-addrs.patch` 保留为最初依赖冲突的最小复现。当前锁文件为补丁后的依赖图。

- 依赖改为 ed25519-dalek 2.2.0、x25519-dalek 2.0.1、curve25519-dalek 4.1.3，配对代码使用 SigningKey／VerifyingKey 和严格签名验证。上述旧版 dalek 条目已从候选锁文件移除；完整依赖审查尚未完成。
- 身份仍序列化为 64 字节，加载时通过 `from_keypair_bytes` 检查公钥是否由私钥派生。往返和错误公钥拒绝测试通过；没有读取或迁移用户既有配对身份。API 依据：[SigningKey](https://docs.rs/ed25519-dalek/2.2.0/ed25519_dalek/struct.SigningKey.html)。
- 读回调直接返回 observed，不调用写回调、不发布写事件。有写回调时，成功仅表示接受意图，不修改缓存。新增 `set_observed_value` 通道更新缓存和事件并绕过写回调，生成器模板与已生成的特征类型同步修改。
- 亮度元数据本身仍未强制范围，保留该特征化测试；实际适配器必须使用 HomeSense 的 Percent 校验。HomeSense 请求规范化已实现 On/Brightness 分组、Percent 校验和可信调用方传入的控制器身份；已通过 LightWriteHandler 和 WriteBatchHandler 接入 HTTP 注册接口，发送前授权及运行时准入尚待接线。

修复后示例编译通过，9 项上游单元测试及 8 项 HomeSense 探针通过。后者包括 [RFC 8032 第 7.1 节](https://www.rfc-editor.org/rfc/rfc8032.html#section-7.1) Ed25519 空消息向量、变更消息拒绝、X25519 双方共享秘密一致性和身份序列化校验。它们不替代 HAP Pair Setup／Pair Verify 端到端测试，也不证明 iPhone 互通。

## 推进条件

1. 在受控源码中升级存在公告的加密依赖，锁定并记录依赖审查。
2. 拆分读、意图提交和观测通知路径；加入请求组及主体上下文；完成范围／错误／背压测试。
3. 实现持久化身份和稳定标识映射，确认加载失败不会悄悄重置。
4. 启动仅含虚拟调光灯的原型，完成 iPhone 配对、On/Brightness、观测通知、重启及撤销配对验收。
5. 满足 A4 多配件桥验收后，才接入已验证的现场设备。

特征写入入口另有 4 项回归：恰好 64 KiB／多一字节、分块累计超限、停滞超时，以及未认证／有效配对／已删除配对的入口检查。测试直接调用 HTTP handler，不代表加密会话端到端验证。分组接口接入进展见下节，仍不能启动现场控制服务。

## HTTP 分组接入

新增 WriteBatchHandler 及 IpServer 注册方法。整组预检通过后只调用一次同步适配器；适配器必须有界、非阻塞且不得重入 HAP 锁。调用后无 await 点，成功才提交会话订阅，不设置特征观测值；失败对整组返回同一 HAP 错误。HomeSense LightWriteHandler 已实现该接口，负责 On/Brightness 规范化并传递控制器身份及运行时拒绝原因。HTTP 回归验证调用次数、背压状态码、观测缓存保持、未知特征整组拒绝，以及不同会话取消订阅的隔离。候选现有 14 项库测试及 8 项探针通过，主 workspace 83 项测试通过。实际持久化准入、加密会话端到端和 iPhone 验收仍待完成。

## 首次配件列表读取

核对 [HAP-NodeJS Characteristic.toHAP](https://github.com/homebridge/HAP-NodeJS/blob/latest/src/lib/Characteristic.ts)：它在列表读取时调用 get handler，错误时回退到默认值。本项目采用快照回调读取，并暂在读取失败时以 HTTP 503 / HAP -70402 拒绝整个列表，不将默认值视为观测。此处是 HomeSense 的原型策略，不能据此声称 Apple 客户端兼容或多配件故障隔离完成。新增列表读取与撤销回归通过，候选共 15+8 项测试。后续需实机确认未知状态的结构发现策略，并完善多配件局部错误处理。

## 加密发送续传

候选原有 TCP 输出在 Pending/短写下可能遗失数据；已保存明文队列头和密文记录的写偏移，保留 nonce 连续性。新回归用故障 socket 与独立解密验证跨三个记录的完整性，并验证 WriteZero 和 nonce 耗尽失败。候选现有 17+8 项测试通过。此修复不等于事件背压完成；队列容量、连接关闭清理、入站分片及握手／加密会话实测仍待验收。

## 出站队列及监听器生命周期

出站队列已限制条数和总字节，超限唤醒并终止会话；HTTP 与加密传输一同释放，会话事件监听器可按稳定 ID 移除。新增 4 项容量／释放回归通过，候选合计 21+8 项测试。仍需完成入站与全局会话预算，并用真实慢 socket 验证关闭、HTTP／通知排序和重连订阅恢复。当前限制及暂未验证的边界见 implementation.md。

## 入站记录解析

替换整包长度相等判断与重复包长消费方式，采用有界 RecordReader 保存残帧和未读完的明文；读取缓存优先于继续等待网络消息。新增 3 项分片、合包、小缓冲与异常记录回归通过。入站队列已设容量，候选合计 24+8 项测试；加密会话切换和真实 socket 验收仍未完成，不能依据纯内存流测试声称 iPhone 可配对。

## 真实 socket 与会话边界

4 项 localhost TCP 测试通过，包括加密双向交换、EOF、输出容量溢出后的关闭，以及密钥早于最终 Pair Verify 响应到达的调度。仅收到会话密钥不再立即公开控制器或加密输出；首个成功解密请求后再启用。坏首帧不发布控制器身份。候选共 28+8 项测试，明确未覆盖真实 Pair Setup／Pair Verify 的密码学消息交换、跨阶段 HTTP pipelining 或 iPhone。继续保持候选状态。

## 会话与认证入口限制

已加入单服务 32 并发连接许可及 60 秒未认证超时，两个监督回归通过。检查认证输入时发现 Pair Verify 原有未经长度检查的切片；现增加有界 checked TLV、严格状态／公钥／标签长度、非低阶 X25519 检查及一次性 Finish 会话消费。三项输入回归通过，候选合计 33+8 项测试。旧 TLV decode 在其他端点仍有调用；Pair Setup 和 pairings 输入审查、正常认证交换及真实客户端兼容性继续作为未完成门槛。
