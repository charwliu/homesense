# NGR 发布构建

发往 NGR 的服务程序和随包隔离验收程序统一使用 Cargo `--release`，在 Mac 本机交叉编译为 `x86_64-unknown-linux-gnu`。不在 NGR 编译或上传源码。

所有构建复用工作区唯一的 `target` 输出目录；目标平台子目录可以保留，不为每轮候选新建构建目录。以下命令须在已配置交叉链接器及固定 Rust 工具链的本机环境执行：

```sh
cargo test --locked --offline --release --target x86_64-unknown-linux-gnu -p homesensed --lib --test unified_hap --no-run --message-format=json
cargo build --locked --offline --release --target x86_64-unknown-linux-gnu -p homesensed --bin homesensed --message-format=json
```

服务制品为 `target/x86_64-unknown-linux-gnu/release/homesensed`。测试制品从本轮 Cargo JSON 的 `compiler-artifact.executable` 读取，不能通过通配符选取旧文件。随包使用的验收客户端也必须显式以 `--release` 构建。测试和示例构建可能连带生成服务 CLI，因此最后单独构建服务，并在生成清单前重新核对所有最终文件指纹。

生成上传清单前，逐个校验 ELF 的 Linux x86-64 架构、输出路径位于目标平台的 `release` 目录，以及 Cargo 产物记录中的 `opt_level = "3"`、`debug_assertions = false`。清单记录源码指纹、构建 profile、文件大小和 SHA-256；构建前后重新核对冻结源码。去掉调试符号不能代替 Release 编译。

新编译的 Release 制品仍需执行对应 Linux 隔离验收，不能沿用旧 Debug 测试程序或旧服务制品的通过记录。编译完成不表示已批准或已完成现网发布；现网切换继续执行备份、恢复守护和部署核对，稳定后仅保留当前及最近一个回退可执行版本。
