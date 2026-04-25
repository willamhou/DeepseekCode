# Rust 技术选型

## 为什么选 Rust

相比 TypeScript 或 Python，Rust 更适合长期型 code CLI：

- 易于分发单文件二进制
- 进程管理、并发、流式输出更稳
- 资源控制和性能更可预期
- 后续扩展 TUI、sandbox、长驻 daemon 更自然

缺点也明确：

- 开发速度相对慢
- LLM agent 现成生态较少
- 很多上层 orchestration 需要自己搭

但如果目标是长期产品而不是临时原型，Rust 是合理选择。

## 推荐 crate

### CLI 与基础设施

- `clap`
- `anyhow`
- `thiserror`
- `tracing`
- `tracing-subscriber`

### 异步与网络

- `tokio`
- `reqwest`
- `futures`

### 数据与配置

- `serde`
- `serde_json`
- `toml`
- `dirs`

### 文件系统与搜索

- `ignore`
- `walkdir`
- `regex`

### 交互与差异展示

- `dialoguer`
- `similar` 或 `diffy`

### 异步 trait 过渡

- `async-trait`

## 首版 crate 使用原则

- 依赖尽量克制
- 不引入重型 agent 框架
- 接口抽象优先于早期过度泛化
- 用简单明确的数据结构跑通闭环

