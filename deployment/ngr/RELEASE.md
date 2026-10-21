# NGR 发布构建

发往 NGR 的运行程序统一使用 Cargo `--release`，在 Mac 本机交叉编译为 `x86_64-unknown-linux-gnu`。NanoPi 使用对应的 `aarch64-unknown-linux-gnu` 目标。不在部署目标编译或上传源码。

全套单元测试、集成测试、模拟器和完整场景回归在本机执行。NGR／NanoPi 只接收部署运行需要的 Release ELF，不上传测试 ELF、测试字节码或测试夹具，也不在目标上执行全套测试。目标侧仅进行启动、健康、架构兼容性及必要的只读部署验收。

所有构建复用工作区唯一的 `target` 输出目录；目标平台子目录可以保留，不为每轮候选新建构建目录。以下命令须在已配置交叉链接器及固定 Rust 工具链的本机环境执行：

```sh
cargo test --locked --offline --release -p homesensed --lib --test unified_hap
cargo build --locked --offline --release --target x86_64-unknown-linux-gnu -p homesensed --bin homesensed --message-format=json
```

服务制品为 `target/x86_64-unknown-linux-gnu/release/homesensed`。上传清单只列本次需要的生产程序，不通过通配符打包整个 `release` 目录。测试和示例构建可能连带生成服务 CLI，因此最后单独构建服务，并在生成清单前重新核对最终文件指纹。

生成上传清单前，逐个校验 ELF 的 Linux x86-64 架构、输出路径位于目标平台的 `release` 目录，以及 Cargo 产物记录中的 `opt_level = "3"`、`debug_assertions = false`。清单记录源码指纹、构建 profile、文件大小和 SHA-256；构建前后重新核对冻结源码。去掉调试符号不能代替 Release 编译。

本机测试记录须对应本轮冻结源码，不能沿用旧 Debug 测试程序或旧服务制品的通过记录。编译完成不表示已批准或已完成现网发布；现网切换继续执行备份、恢复守护和部署核对，稳定后仅保留当前及最近一个回退可执行版本。

清理构建空间时，先核对正在运行的构建和待部署制品，保留所需 ELF、清单与验收证据；旧 Debug、增量缓存及历史构建输出可删除。不得为每轮候选另建构建输出目录。
