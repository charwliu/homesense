# ADR 0001：原生 Rust HAP 候选核查

日期：2026-10-10。状态：候选尚未通过接入验收，原生 Rust HAP 架构不变。

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

HTTP body 聚合、握手／会话限制、权限撤销和事件背压还需核查和回归。当前没有启动局域网服务，没有连接真实总线，没有 iPhone 配对证据。

## 受控补丁进展

`candidate.patch` 相对于固定上游提交包含编译兼容及 API 修复，`check.sh` 已应用此补丁；`if-addrs.patch` 保留为最初依赖冲突的最小复现。当前锁文件为补丁后的依赖图。

- 依赖改为 ed25519-dalek 2.2.0、x25519-dalek 2.0.1、curve25519-dalek 4.1.3，配对代码使用 SigningKey／VerifyingKey 和严格签名验证。上述旧版 dalek 条目已从候选锁文件移除；完整依赖审查尚未完成。
- 身份仍序列化为 64 字节，加载时通过 `from_keypair_bytes` 检查公钥是否由私钥派生。往返和错误公钥拒绝测试通过；没有读取或迁移用户既有配对身份。API 依据：[SigningKey](https://docs.rs/ed25519-dalek/2.2.0/ed25519_dalek/struct.SigningKey.html)。
- 读回调直接返回 observed，不调用写回调、不发布写事件。有写回调时，成功仅表示接受意图，不修改缓存。新增 `set_observed_value` 通道更新缓存和事件并绕过写回调，生成器模板与已生成的特征类型同步修改。
- 亮度元数据本身仍未强制范围，保留该特征化测试；实际适配器必须使用 HomeSense 的 Percent 校验。HomeSense 请求规范化已实现 On/Brightness 分组、Percent 校验和可信调用方传入的控制器身份；尚未接入 HTTP 入口及会话鉴权。

修复后示例编译通过，9 项上游单元测试及 8 项 HomeSense 探针通过。后者包括 [RFC 8032 第 7.1 节](https://www.rfc-editor.org/rfc/rfc8032.html#section-7.1) Ed25519 空消息向量、变更消息拒绝、X25519 双方共享秘密一致性和身份序列化校验。它们不替代 HAP Pair Setup／Pair Verify 端到端测试，也不证明 iPhone 互通。

## 推进条件

1. 在受控源码中升级存在公告的加密依赖，锁定并记录依赖审查。
2. 拆分读、意图提交和观测通知路径；加入请求组及主体上下文；完成范围／错误／背压测试。
3. 实现持久化身份和稳定标识映射，确认加载失败不会悄悄重置。
4. 启动仅含虚拟调光灯的原型，完成 iPhone 配对、On/Brightness、观测通知、重启及撤销配对验收。
5. 满足 A4 多配件桥验收后，才接入已验证的现场设备。
