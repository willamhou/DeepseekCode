# DeepseekCode VS Code Extension

VS Code entrypoint for the `deepseek` CLI.

The extension adds a `DeepseekCode` Explorer view, status bar action, editor title action, command palette commands, and editor context menu entries for common workflows.

## Commands

- `DeepseekCode: Quick Action` opens a quick-pick menu for common workflows
- `DeepseekCode: Open Chat` launches `deepseek`
- `DeepseekCode: Run Task` prompts for a task and runs `deepseek run`
- `DeepseekCode: Explain Selection` sends the active file and selected text as task context
- `DeepseekCode: Run Benchmark` runs `deepseek benchmark`
- `DeepseekCode: Show Dogfood Report` runs `deepseek dogfood report --limit 10`

## Explorer View

The `DeepseekCode` view in the Explorer sidebar exposes the same core actions as clickable items, so common agent workflows are available without opening the command palette.

## Settings

- `deepseek.command`: command used to launch the CLI. Default: `deepseek`
- `deepseek.maxSelectionChars`: maximum selected text included in task prompts. Default: `6000`

For local development, open this folder in VS Code and run the extension host.
