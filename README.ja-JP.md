# DeepSeekCode

[English](./README.md) | [中文](./README.zh-CN.md) | [日本語](./README.ja-JP.md)

DeepSeekCode は DeepSeek-first のターミナル向けコーディングエージェントであり、
ローカル TUI/runtime workbench です。実際の開発ループ、つまりリポジトリを読む、
ファイルを編集する、チェックを走らせる、結果を確認する、そして同じターミナルで
反復する流れを前提に作られています。

> 状態: dogfood とリポジトリ内の開発作業には利用できます。`v0.1.0` では
> GitHub Release のバイナリと GHCR イメージを公開済みです。npm と Homebrew
> の公開には registry/tap の資格情報がまだ必要で、ネイティブ PTY と製品面の
> 磨き込みは継続中です。

<p align="center">
  <img src="./docs/demo/deepseek-code-tui-demo.svg" alt="DeepSeekCode animated TUI demo recording" width="100%">
</p>

## 現在できること

- `deepseek run` で単発のコーディングタスクを実行できます。
- `deepseek tui` でキーボード操作のターミナル workbench を開き、Plan / Agent /
  YOLO モードを切り替えられます。
- `.dscode/runtime/` 以下に sessions、threads、turns、items、events、tasks、
  usage、automations を永続化します。
- ファイルの読み取り/検索、パッチ適用、diff review、todo、rollback snapshot、
  notes、memory、hooks、skills、subagents を扱えます。
- 権限ゲート付き shell 実行に加えて、バックグラウンド shell job、wait/poll、
  replay、attach snapshot、stdin、resize metadata、cancel、workspace
  shell-supervisor protocol bridge をサポートします。
- ローカル HTTP/SSE runtime、ACP stdio adapter、MCP client/server tooling、
  TUI 内の MCP 管理画面を備えています。
- RLM helper による再帰/長文入力の分析、model-session context、live queue
  status、event replay、cancel、recover、drain control をサポートします。
- LSP-backed diagnostics と fallback diagnostics を実行でき、JSON/JSONL watch
  出力にも対応します。
- Linux x64、macOS x64、macOS arm64、Windows x64 の Release assets、
  GHCR イメージ、npm/Homebrew 公開用メタデータがあります。

## クイックスタート

ソースからインストール:

```bash
cargo install --git https://github.com/willamhou/DeepSeekCode.git --locked
deepseek version
deepseek doctor --json
```

または release archive をダウンロード:

```bash
curl -L -o deepseek-linux-x64.tar.gz \
  https://github.com/willamhou/DeepSeekCode/releases/download/v0.1.0/deepseek-linux-x64.tar.gz
curl -L -o deepseek-linux-x64.tar.gz.sha256 \
  https://github.com/willamhou/DeepSeekCode/releases/download/v0.1.0/deepseek-linux-x64.tar.gz.sha256
shasum -a 256 -c deepseek-linux-x64.tar.gz.sha256
tar -xzf deepseek-linux-x64.tar.gz
./deepseek version
```

または公開済みコンテナを実行:

```bash
docker run --rm ghcr.io/willamhou/deepseekcode:0.1.0 version
```

またはローカル checkout からインストール:

```bash
cargo install --path .
deepseek config init
printf '%s\n' '<api-key>' | deepseek config auth DEEPSEEK_API_KEY --stdin
deepseek doctor --json
```

コーディングタスクを実行:

```bash
deepseek run "explain the current repository structure"
```

TUI を起動:

```bash
deepseek tui
deepseek tui --demo --once
```

ローカル runtime を起動し、TUI から接続:

```bash
deepseek serve --http --addr 127.0.0.1:13000
deepseek tui --runtime-url http://127.0.0.1:13000
```

実モデル呼び出しには `DEEPSEEK_API_KEY` を設定してください。ローカルの `.env`
ファイルは git から無視されます。

## 現在の差分

DeepSeekCode は自身の開発に使える段階ですが、Claude Code CLI / Codex CLI
ほどの製品成熟度にはまだ届いていません。大きな残差は次の通りです。

- ネイティブ supervisor-owned PTY の attach/stdin/resize/replay/wait/cancel。
- 実リポジトリを使った live external write-fixture 検証。
- npm registry 公開と Homebrew tap。どちらも資格情報が未設定です。
- CLI stdin auth と guided `/setup` を超える TUI 内 masked credential
  wizard、より実運用に近い model-backed demo の製品化。

## Demo 素材

README の demo 画像は決定的な TUI snapshot から生成した animated SVG です。

```bash
svg-term --command "bash -lc 'target/debug/deepseek tui --demo --once | sed -e \"s/^\\\"//\" -e \"s/\\\"$//\" | while IFS= read -r line; do printf \"%s\\n\" \"\$line\"; sleep 0.08; done; sleep 1.5'" \
  --out docs/demo/deepseek-code-tui-demo.svg \
  --width 122 \
  --height 36 \
  --window \
  --no-cursor
```

`docs/demo/deepseek-code-tui.svg` は静的 snapshot として残しています。公開品質の
リリースでは、実モデルを使ったループも短い GIF/MP4 として追加します: TUI を開く、
コーディングリクエストを送る、編集を適用する、テストを走らせる、diff を確認する、
という流れです。生成したメディアは `docs/demo/` に置きます。

## 開発チェック

```bash
cargo fmt --check
cargo test --lib -- --test-threads=1
cargo package --allow-dirty
deepseek tui --demo --once
```

npm wrapper メタデータ:

```bash
node npm/scripts/check-version-sync.js
```

リリース準備状態:

```bash
deepseek update publish-status
deepseek update publish-status --dist dist-assets --npm-dist npm-dist --strict
deepseek update publish-status --json
```

PR/CI workflow チェック:

```bash
deepseek pr live-status owner/repo#42
deepseek pr live-status owner/repo#42 --require-write
deepseek pr live-status owner/repo#42 --json
```

## ドキュメント

- [Install](./docs/install.md)
- [Architecture](./docs/architecture.md)
- [Runtime contract](./docs/runtime.md)
- [TUI workbench](./docs/tui.md)
- [REPL mode](./docs/repl.md)
- [Streaming](./docs/streaming.md)
- [Agent tasks](./docs/agents.md)
- [Todo tool](./docs/todos.md)
- [PR / CI integration](./docs/pr-integration.md)
- [Release checklist](./docs/release.md)
- [Roadmap](./docs/roadmap.md)
- [Changelog](./CHANGELOG.md)

## リポジトリについて

このリポジトリは透明性と共同作業のために公開されています。公開されていることは、
[LICENSE](./LICENSE) に記載された条件を超える追加のオープンソース許諾を意味しません。

ローカル資格情報、API key、runtime state、非公開の `.env` ファイルをコミットしないで
ください。追跡されているサンプルはプレースホルダーのみを使っています。
