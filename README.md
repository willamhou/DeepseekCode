# DeepSeek Code

`DeepSeek Code` 是一个 `DeepSeek-first` 的终端代码代理项目。

目标不是做一个“支持所有模型”的通用外壳，而是做一个对 `DeepSeek` 深度优化、对大多数编程语言仓库可用的 code CLI。第一阶段聚焦本地仓库中的核心闭环：

- 读代码
- 搜索代码
- 生成并应用补丁
- 运行受控命令
- 根据结果继续修复

## 产品定位

一句话定义：

> 一个使用 Rust 构建的 DeepSeek-first code CLI，面向大多数主流语言仓库，完成“理解代码 -> 修改代码 -> 执行验证 -> 迭代修复”的闭环。

关键边界：

- 只优先适配 `DeepSeek`
- 覆盖大多数文本型代码仓库
- 对 `TypeScript/JavaScript`、`Python`、`Go`、`Java`、`Rust` 做重点优化
- 第一版不做重型插件平台
- 第一版不承诺复杂 AST 级重构

## 为什么只做 DeepSeek-first

多模型接入并不难，难的是让不同模型在以下环节都稳定：

- 工具调用格式
- 长任务一致性
- patch 输出质量
- shell 决策安全性
- 上下文裁剪后的表现

因此本项目的策略是：

- 底层架构保留扩展空间
- 第一阶段产品体验收敛到 `DeepSeek`
- 先把 agent loop、tooling、patch、approval 做稳

## 语言支持策略

语言支持分三层：

### L1 通用支持

对大多数文本型代码仓库统一支持：

- 目录扫描
- 文件读取
- 全文搜索
- patch 编辑
- 运行项目已有命令
- 基于报错继续修复

### L2 主流语言增强

第一批重点增强：

- TypeScript / JavaScript
- Python
- Go
- Java
- Rust

增强点包括：

- 仓库类型识别
- 默认 test/lint/build/typecheck 命令推断
- 文件优先级策略
- 常见目录与忽略规则

### L3 后续特化

后续再逐步补充：

- C/C++
- C#
- PHP
- Ruby
- Kotlin
- Swift

## Skill 与 Profile

本项目不打算一开始做复杂插件系统，而是先做轻量策略层：

- `Language Profile`
  面向仓库/语言，定义命令推断、优先文件、忽略规则等
- `Skill`
  面向任务，定义提示词补充、工具白名单、执行策略、审批策略等

第一阶段用本地 `TOML` 文件加载，优先保持简单、安全、可测试。

## v0.1 范围

首版目标是跑通最小可用闭环：

- 交互式会话
- 单次任务执行
- 项目扫描
- 文件读取与搜索
- 统一 patch 编辑
- 受控 shell 执行
- diff 展示
- 会话恢复
- 配置管理

首版不做：

- 多模型适配
- IDE 插件
- PR review / CI 云端集成
- 多 agent 并行
- 远程 skill 市场

## 文档

- [安装指南](./docs/install.md)
- [架构设计](./docs/architecture.md)
- [MVP 与路线图](./docs/mvp.md)
- [状态与完整 Roadmap](./docs/roadmap.md)
- [Skill 与 Language Profile 设计](./docs/skills-and-profiles.md)
- [Rust 技术选型](./docs/rust-stack.md)
- [PR / CI 集成指南](./docs/pr-integration.md)
- [REPL 模式 (`deepseek` / `deepseek chat`)](./docs/repl.md)

## 快速开始

```bash
cargo install --path .
deepseek version
deepseek
```

升级源码安装：

```bash
git pull
cargo install --path . --force
deepseek version
```

如果想启用 shell completion：

```bash
deepseek completion bash > ~/.local/share/bash-completion/completions/deepseek
deepseek completion zsh > ~/.zfunc/_deepseek
deepseek completion fish > ~/.config/fish/completions/deepseek.fish
```
