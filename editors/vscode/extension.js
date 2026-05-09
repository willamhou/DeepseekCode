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
    vscode.commands.registerCommand("deepseek.quickAction", quickAction),
    vscode.commands.registerCommand("deepseek.openChat", openChat),
    vscode.commands.registerCommand("deepseek.runTask", runTask),
    vscode.commands.registerCommand("deepseek.explainSelection", explainSelection),
    vscode.commands.registerCommand("deepseek.runBenchmark", runBenchmark),
    vscode.commands.registerCommand("deepseek.showDogfoodReport", showDogfoodReport),
  );
}

function deactivate() {}

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

module.exports = {
  activate,
  deactivate,
  shellQuote,
};
