# DeepseekCode VS Code Extension

Minimal VS Code entrypoint for the `deepseek` CLI.

## Commands

- `DeepseekCode: Open Chat` launches `deepseek`
- `DeepseekCode: Run Task` prompts for a task and runs `deepseek run`
- `DeepseekCode: Explain Selection` sends the active file and selected text as task context
- `DeepseekCode: Run Benchmark` runs `deepseek benchmark`
- `DeepseekCode: Show Dogfood Report` runs `deepseek dogfood report --limit 10`

## Settings

- `deepseek.command`: command used to launch the CLI. Default: `deepseek`
- `deepseek.maxSelectionChars`: maximum selected text included in task prompts. Default: `6000`

For local development, open this folder in VS Code and run the extension host.
