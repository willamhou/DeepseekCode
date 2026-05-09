# 安装

`deepseek` 是默认命令名。推荐先安装，再用 `deepseek version` 和 `deepseek doctor` 做最小验证。

## 从源码安装

```bash
cargo install --path .
deepseek version
deepseek doctor
```

如果你只想先本地构建 release binary：

```bash
cargo build --release
./target/release/deepseek version
```

## 发布前检查

本仓库的最小 release gate 是：

```bash
cargo fmt --check
cargo test
deepseek benchmark
deepseek version
deepseek doctor
```

`deepseek benchmark` 会同时检查：

- benchmark case expectations
- benchmark trend gate
- dogfood live gate

任一 gate 失败都应阻断 release。

## Release Binary

本地 release binary 路径固定为：

```bash
cargo build --release
./target/release/deepseek version
```

发布产物至少应包含：

- `deepseek` binary
- 对应 commit SHA
- `deepseek version` 输出
- 支持的平台说明
- 安装与升级说明链接

`dscode` 只作为兼容别名，不作为主 release artifact 名称。

## 升级

从源码升级：

```bash
git pull
cargo install --path . --force
deepseek version
deepseek doctor
```

如果是使用 release binary 升级，先保留当前版本：

```bash
mkdir -p ~/.local/bin/deepseek-rollback
cp "$(command -v deepseek)" ~/.local/bin/deepseek-rollback/deepseek.previous
```

然后替换 binary，并验证：

```bash
deepseek version
deepseek doctor
```

配置文件和会话默认保存在 `.dscode/`，升级 binary 不应删除这些文件。

## 回滚

如果升级后需要回滚 release binary：

```bash
cp ~/.local/bin/deepseek-rollback/deepseek.previous "$(command -v deepseek)"
deepseek version
deepseek doctor
```

如果是从源码安装，回滚到指定 commit：

```bash
git checkout <known-good-commit>
cargo install --path . --force
deepseek version
```

## 首次配置

```bash
cp .dscode/config.example.toml .dscode/config.toml
deepseek doctor
```

如果要做一次最小 live 请求验证：

```bash
deepseek smoke
```

## 基本用法

- `deepseek`：直接进入交互模式
- `deepseek "task"` 或 `deepseek run "task"`：执行单次任务
- `deepseek benchmark`：跑本地 benchmark 基线
- `deepseek dogfood ...`：记录或回放真实任务
- `deepseek completion bash|zsh|fish`：生成 shell completion 脚本

## Shell Completion

```bash
mkdir -p ~/.local/share/bash-completion/completions
deepseek completion bash > ~/.local/share/bash-completion/completions/deepseek
```

```bash
mkdir -p ~/.zfunc
deepseek completion zsh > ~/.zfunc/_deepseek
```

```bash
mkdir -p ~/.config/fish/completions
deepseek completion fish > ~/.config/fish/completions/deepseek.fish
```

`dscode` 仍可作为兼容别名使用，但主文档和主命令统一为 `deepseek`。
