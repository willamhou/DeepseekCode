const path = require("path");
const vscode = require("vscode");

function activate(context) {
  const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  status.text = "$(sparkle) DeepseekCode";
  status.tooltip = "DeepseekCode actions";
  status.command = "deepseek.quickAction";
  status.show();

  context.subscriptions.push(
    status,
    vscode.window.registerTreeDataProvider("deepseek.actions", new DeepseekActionsProvider()),
    vscode.window.registerWebviewViewProvider("deepseek.panel", new DeepseekPanelProvider()),
    vscode.commands.registerCommand("deepseek.quickAction", quickAction),
    vscode.commands.registerCommand("deepseek.openPanel", openPanel),
    vscode.commands.registerCommand("deepseek.openChat", openChat),
    vscode.commands.registerCommand("deepseek.runTask", runTask),
    vscode.commands.registerCommand("deepseek.explainSelection", explainSelection),
    vscode.commands.registerCommand("deepseek.runBenchmark", runBenchmark),
    vscode.commands.registerCommand("deepseek.showDogfoodReport", showDogfoodReport),
  );
}

function deactivate() {}

class DeepseekPanelProvider {
  resolveWebviewView(view) {
    view.webview.options = {
      enableScripts: true,
    };
    view.webview.html = panelHtml(nonce());
    view.webview.onDidReceiveMessage(async (message) => {
      switch (message?.type) {
        case "openChat":
          await openChat();
          break;
        case "runTask":
          await runPanelTask(message.task);
          break;
        case "explainSelection":
          await explainSelection();
          break;
        case "runBenchmark":
          await runBenchmark();
          break;
        case "showDogfoodReport":
          await showDogfoodReport();
          break;
      }
    });
  }
}

class DeepseekActionsProvider {
  getTreeItem(item) {
    return item;
  }

  getChildren() {
    return [
      actionItem(
        "Open Chat",
        "Start an interactive session",
        "deepseek.openChat",
        "comment-discussion",
      ),
      actionItem("Run Task", "Prompt for a workspace task", "deepseek.runTask", "terminal"),
      actionItem(
        "Explain Selection",
        "Send active file and selection as context",
        "deepseek.explainSelection",
        "symbol-method",
      ),
      actionItem(
        "Run Benchmark",
        "Run the local benchmark suite",
        "deepseek.runBenchmark",
        "beaker",
      ),
      actionItem(
        "Show Dogfood Report",
        "Show recent dogfood runs",
        "deepseek.showDogfoodReport",
        "graph",
      ),
    ];
  }
}

function actionItem(label, description, command, codicon) {
  const item = new vscode.TreeItem(label, vscode.TreeItemCollapsibleState.None);
  item.description = description;
  item.tooltip = description;
  item.iconPath = new vscode.ThemeIcon(codicon);
  item.command = {
    command,
    title: label,
  };
  return item;
}

function panelHtml(panelNonce) {
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${panelNonce}';">
  <style>
    body {
      box-sizing: border-box;
      color: var(--vscode-foreground);
      font-family: var(--vscode-font-family);
      margin: 0;
      padding: 12px;
    }
    .stack {
      display: flex;
      flex-direction: column;
      gap: 8px;
    }
    textarea {
      background: var(--vscode-input-background);
      border: 1px solid var(--vscode-input-border);
      box-sizing: border-box;
      color: var(--vscode-input-foreground);
      font-family: var(--vscode-font-family);
      min-height: 96px;
      padding: 8px;
      resize: vertical;
      width: 100%;
    }
    button {
      align-items: center;
      background: var(--vscode-button-secondaryBackground);
      border: 0;
      color: var(--vscode-button-secondaryForeground);
      cursor: pointer;
      display: flex;
      font: inherit;
      justify-content: center;
      min-height: 28px;
      padding: 5px 8px;
      text-align: center;
      width: 100%;
    }
    button.primary {
      background: var(--vscode-button-background);
      color: var(--vscode-button-foreground);
    }
    button:hover {
      background: var(--vscode-button-hoverBackground);
    }
    .grid {
      display: grid;
      gap: 8px;
      grid-template-columns: 1fr 1fr;
    }
  </style>
</head>
<body>
  <div class="stack">
    <textarea id="task" aria-label="Task" placeholder="Task"></textarea>
    <button class="primary" id="run">Run</button>
    <div class="grid">
      <button id="chat">Chat</button>
      <button id="explain">Explain</button>
      <button id="benchmark">Benchmark</button>
      <button id="dogfood">Dogfood</button>
    </div>
  </div>
  <script nonce="${panelNonce}">
    const vscode = acquireVsCodeApi();
    const task = document.getElementById("task");
    document.getElementById("run").addEventListener("click", () => {
      vscode.postMessage({ type: "runTask", task: task.value });
    });
    document.getElementById("chat").addEventListener("click", () => {
      vscode.postMessage({ type: "openChat" });
    });
    document.getElementById("explain").addEventListener("click", () => {
      vscode.postMessage({ type: "explainSelection" });
    });
    document.getElementById("benchmark").addEventListener("click", () => {
      vscode.postMessage({ type: "runBenchmark" });
    });
    document.getElementById("dogfood").addEventListener("click", () => {
      vscode.postMessage({ type: "showDogfoodReport" });
    });
  </script>
</body>
</html>`;
}

function config() {
  return vscode.workspace.getConfiguration("deepseek");
}

function deepseekCommand() {
  return config().get("command", "deepseek").trim() || "deepseek";
}

function maxSelectionChars() {
  return config().get("maxSelectionChars", 6000);
}

function workspaceCwd() {
  const editor = vscode.window.activeTextEditor;
  if (editor) {
    const folder = vscode.workspace.getWorkspaceFolder(editor.document.uri);
    if (folder) {
      return folder.uri.fsPath;
    }
    if (editor.document.uri.scheme === "file") {
      return path.dirname(editor.document.uri.fsPath);
    }
  }

  const firstFolder = vscode.workspace.workspaceFolders?.[0];
  return firstFolder?.uri.fsPath;
}

function runInTerminal(command) {
  const terminal = vscode.window.createTerminal({
    name: "DeepseekCode",
    cwd: workspaceCwd(),
  });
  terminal.show(true);
  terminal.sendText(command);
}

async function quickAction() {
  const hasSelection = Boolean(
    vscode.window.activeTextEditor && !vscode.window.activeTextEditor.selection.isEmpty,
  );
  const picked = await vscode.window.showQuickPick(
    [
      {
        label: "$(layout-sidebar-right) Open Agent Panel",
        description: "Focus the DeepseekCode sidebar task panel",
        command: "deepseek.openPanel",
      },
      {
        label: "$(comment-discussion) Open Chat",
        description: "Start an interactive DeepseekCode session",
        command: "deepseek.openChat",
      },
      {
        label: "$(terminal) Run Task",
        description: "Prompt for a task in the current workspace",
        command: "deepseek.runTask",
      },
      {
        label: "$(symbol-method) Explain Selection",
        description: hasSelection
          ? "Send selected code as context"
          : "Uses the active file path as context",
        command: "deepseek.explainSelection",
      },
      {
        label: "$(beaker) Run Benchmark",
        description: "Run the local benchmark suite",
        command: "deepseek.runBenchmark",
      },
      {
        label: "$(graph) Show Dogfood Report",
        description: "Show recent dogfood runs",
        command: "deepseek.showDogfoodReport",
      },
    ],
    {
      title: "DeepseekCode",
      placeHolder: workspaceCwd() || "No workspace folder open",
      ignoreFocusOut: true,
    },
  );
  if (picked) {
    await vscode.commands.executeCommand(picked.command);
  }
}

async function openPanel() {
  await vscode.commands.executeCommand("deepseek.panel.focus");
}

async function openChat() {
  runInTerminal(deepseekCommand());
}

async function runTask() {
  const task = await vscode.window.showInputBox({
    title: "DeepseekCode Task",
    prompt: "Task to run in the current workspace",
    ignoreFocusOut: true,
  });
  if (!task || !task.trim()) {
    return;
  }

  runInTerminal(`${deepseekCommand()} run ${shellQuote(promptWithEditorContext(task.trim()))}`);
}

async function runPanelTask(task) {
  if (!task || !task.trim()) {
    vscode.window.showInformationMessage("Enter a DeepseekCode task before running.");
    return;
  }

  runInTerminal(`${deepseekCommand()} run ${shellQuote(promptWithEditorContext(task.trim()))}`);
}

async function explainSelection() {
  const prompt = promptWithEditorContext("Explain this code and point out correctness risks.");
  if (!prompt) {
    vscode.window.showInformationMessage("Open a file or select code before running this command.");
    return;
  }

  runInTerminal(`${deepseekCommand()} run ${shellQuote(prompt)}`);
}

async function runBenchmark() {
  runInTerminal(`${deepseekCommand()} benchmark`);
}

async function showDogfoodReport() {
  runInTerminal(`${deepseekCommand()} dogfood report --limit 10`);
}

function promptWithEditorContext(task) {
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    return task;
  }

  const relativePath = relativeDocumentPath(editor.document);
  const selectionText = selectedText(editor);
  const contextParts = [];

  if (relativePath) {
    contextParts.push(`File: ${relativePath}`);
  }
  if (selectionText) {
    contextParts.push(`Selection:\n${selectionText}`);
  }

  if (contextParts.length === 0) {
    return task;
  }
  return `${task}\n\n${contextParts.join("\n\n")}`;
}

function relativeDocumentPath(document) {
  if (document.uri.scheme !== "file") {
    return undefined;
  }
  const folder = vscode.workspace.getWorkspaceFolder(document.uri);
  if (!folder) {
    return document.uri.fsPath;
  }
  return path.relative(folder.uri.fsPath, document.uri.fsPath);
}

function selectedText(editor) {
  if (editor.selection.isEmpty) {
    return "";
  }
  const raw = editor.document.getText(editor.selection);
  const limit = maxSelectionChars();
  if (raw.length <= limit) {
    return raw;
  }
  return `${raw.slice(0, limit)}\n[truncated after ${limit} characters]`;
}

function shellQuote(value) {
  return `'${String(value).replace(/'/g, `'\\''`)}'`;
}

function nonce() {
  let value = "";
  const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  for (let index = 0; index < 32; index += 1) {
    value += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return value;
}

module.exports = {
  activate,
  deactivate,
  shellQuote,
};
