# MVP 与路线图

## v0.1 目标

`v0.1` 的目标不是“最强 code agent”，而是“稳定跑通本地代码修改闭环”。

主命令以 `deepseek` 为准；`dscode` 仍保留为兼容别名。

一句话：

> 在本地仓库中，能够围绕 DeepSeek 完成读代码、改代码、跑命令、继续修复的基本代理流程。

## v0.1 功能清单

### 基础交互

- `deepseek`
- `deepseek "task"`
- `deepseek diff`
- `deepseek resume`
- `deepseek config`
- `deepseek doctor`

### 项目理解

- 扫描目录结构
- 检测主要语言
- 推断包管理器与常见命令
- 识别忽略目录

### 工具能力

- `list_files`
- `read_file`
- `search_text`
- `apply_patch`
- `run_shell`
- `git_diff`

### Runtime 能力

- agent loop
- 上下文裁剪
- 会话保存和恢复
- diff 展示
- 审批策略

## 首版不做

- 多模型 provider
- IDE 插件
- 远程 skill 安装
- 多 agent 并行
- 自动提交和推送 git
- AST 级大规模重构

## 开发阶段建议

### Phase 1: 基础骨架

- 初始化 CLI
- 接入配置加载
- 接入 DeepSeek API
- 打通最简单的单轮问答

### Phase 2: Tool 闭环

- 实现文件读取与搜索
- 实现 patch 应用
- 实现 shell 执行
- 跑通模型请求工具 -> 工具执行 -> 模型继续

### Phase 3: 仓库策略

- 语言检测
- profile 加载
- 命令推断
- 忽略规则

### Phase 4: 任务策略

- skill 加载
- 工具白名单
- shell allowlist
- skill-based prompt augmentation

### Phase 5: 体验打磨

- diff 渲染
- 流式输出
- 会话恢复
- `doctor` 命令

## 验收标准

如果下面几类任务能稳定完成，`v0.1` 就是成立的：

- “解释这个模块是干什么的”
- “修复这个 failing test”
- “修复 lint / typecheck 错误”
- “基于报错做一轮小范围修改”
- “给一个函数加一处小功能并跑验证命令”
