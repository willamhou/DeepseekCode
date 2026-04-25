# 架构设计

## 总体原则

架构按四层划分：

1. `CLI / UI`
2. `Core Runtime`
3. `Model Adapter`
4. `Tooling + Strategy`

设计目标：

- 模型适配与 agent 运行时解耦
- 工具原子化，方便权限控制和审计
- `Skill` 与 `Language Profile` 作为策略层存在，不污染核心运行时
- 先保证闭环稳定，再做生态扩展

## 目录建议

```text
DeepSeek Code/
  Cargo.toml
  src/
    main.rs

    cli/
      mod.rs
      app.rs
      commands/
        chat.rs
        run.rs
        diff.rs
        resume.rs
        config.rs
        doctor.rs

    core/
      mod.rs
      agent.rs
      loop.rs
      planner.rs
      executor.rs
      memory.rs
      session.rs
      context.rs
      approval.rs

    model/
      mod.rs
      client.rs
      deepseek.rs
      protocol.rs
      stream.rs

    tools/
      mod.rs
      registry.rs
      types.rs
      read_file.rs
      list_files.rs
      search_text.rs
      apply_patch.rs
      run_shell.rs
      git_diff.rs

    language/
      mod.rs
      detect.rs
      profile.rs
      infer.rs
      profiles/
        generic.rs
        rust.rs
        python.rs
        typescript.rs
        go.rs
        java.rs

    skills/
      mod.rs
      schema.rs
      loader.rs
      registry.rs
      resolver.rs

    config/
      mod.rs
      types.rs
      load.rs
      paths.rs

    ui/
      mod.rs
      render.rs
      diff.rs
      confirm.rs
      stream.rs

    error/
      mod.rs

  skills/
  profiles/
```

## 分层说明

### CLI / UI

职责：

- 解析参数
- 呈现交互式会话
- 展示 diff、确认框、流式输出
- 提供 `chat/run/diff/resume/config/doctor` 命令

### Core Runtime

职责：

- 维护 agent loop
- 管理上下文和记忆
- 驱动工具调用
- 处理 approval policy
- 保存会话状态

`Core` 应尽量不依赖具体模型供应商。

### Model Adapter

职责：

- 与 DeepSeek API 通信
- 统一消息格式
- 解析模型输出
- 适配流式输出与 tool-call 风格

这里先只做 `DeepSeek`，但接口上保留扩展空间。

### Tooling + Strategy

职责：

- 提供可执行工具
- 识别语言与仓库类型
- 加载 `Skill` 和 `Language Profile`
- 选择合理命令与文件优先级

## 核心运行循环

第一版建议采用简单 loop：

1. 用户输入任务
2. 检测语言 profile
3. 解析可选 skill
4. 组装 system prompt
5. 调用模型
6. 如果模型请求工具，则执行工具
7. 将工具结果回填模型
8. 循环直到完成或达到 step limit
9. 展示变更 diff 和最终总结

第一版不要过早把 planner/executor 做得很重，`loop.rs` 可以先承载主流程。

## 核心抽象

建议保留两个基础 trait：

```rust
pub trait ModelClient {
    async fn respond(&self, input: ModelRequest) -> anyhow::Result<ModelResponse>;
}
```

```rust
pub trait Tool {
    fn name(&self) -> &'static str;
    async fn execute(&self, input: ToolInput) -> anyhow::Result<ToolOutput>;
}
```

统一通过 `ToolRegistry` 做分发和权限控制。

## 关键工程原则

- 文件修改优先走 patch，不做整文件覆盖式写入
- shell 执行必须受控，并支持审批
- 所有工具调用都要保留日志和可审计记录
- 策略配置数据驱动，避免把语言和任务逻辑写死在 prompt 里

