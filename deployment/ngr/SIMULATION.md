# HomeSense 本机模拟

完整模拟器和场景回归在开发本机运行，复用仓库根 `target/`，使用独立状态目录和合成身份。模拟网关使用内存或回环地址，不连接家庭总线。

```sh
cargo run --locked -p homesensed -- check-config config/simulation.json --simulation
cargo run --locked -p homesensed -- check-automation config/simulation.json config/automation.simulation.json --simulation
cargo run --locked -p homesensed -- serve-automation-simulation config/simulation.json config/automation.simulation.json /tmp/homesense-automation-demo 10
```

测试文件、测试 ELF、字节码和模拟器不上传 NGR 或 NanoPi。部署目标只接收运行所需生产 Release ELF 及明确授权的资料，执行启动、健康与必要只读验收。构建与验证规则见[实现指南](../../docs/implementation.md)及[项目工作规则](../../AGENTS.md)。
