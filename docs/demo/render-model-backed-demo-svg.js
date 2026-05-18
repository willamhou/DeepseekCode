#!/usr/bin/env node
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const { validateTranscript } = require("./verify-model-backed-demo.js");

const usage = `Usage: docs/demo/render-model-backed-demo-svg.js [--self-test] [--out <svg>] <transcript.log>

Renders a verified model-backed demo transcript into a static SVG suitable for
README review. The input transcript must pass verify-model-backed-demo.js first.`;

function fail(message) {
  throw new Error(message);
}

function escapeXml(value) {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function stripAnsi(value) {
  return value.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "");
}

function normalizeExecCommand(line) {
  return line
    .replace(/^\$.*?\bdeepseek(\.exe)?\s+exec\b/i, "$ deepseek$1 exec")
    .replace(/"[^"]*"/, '"<prompt>"');
}

function lineAt(lines, startIndex, predicate) {
  for (let index = startIndex; index < lines.length; index += 1) {
    if (predicate(lines[index])) {
      return { index, value: lines[index] };
    }
  }
  return { index: -1, value: "" };
}

function cleanLine(line) {
  return stripAnsi(line).replace(/\t/g, "  ").replace(/\s+$/g, "");
}

function collectHighlights(transcript) {
  validateTranscript(transcript);
  const lines = transcript.replace(/\r\n/g, "\n").split("\n").map(cleanLine);
  const initial = lineAt(lines, 0, (line) => line === "$ cargo test");
  const exec = lineAt(lines, initial.index + 1, (line) => /\bdeepseek(?:\.exe)?\s+exec\b/i.test(line));
  const diff = lineAt(lines, exec.index + 1, (line) => line === "$ git diff -- src/lib.rs");
  const final = lineAt(lines, diff.index + 1, (line) => line === "$ cargo test");

  if ([initial, exec, diff, final].some((marker) => marker.index < 0)) {
    fail("verified transcript markers could not be extracted");
  }

  const output = ["DeepSeekCode model-backed coding demo", "", initial.value];
  const failedLines = lines
    .slice(initial.index + 1, exec.index)
    .filter((line) => /\.\.\.\s+FAILED\b|test result:\s+FAILED/i.test(line))
    .slice(0, 3);
  output.push(...failedLines, "", normalizeExecCommand(exec.value));

  const agentLines = lines
    .slice(exec.index + 1, diff.index)
    .filter((line) => line && !line.startsWith("$") && !/^transcript:|^demo repo:/i.test(line))
    .slice(0, 4);
  if (agentLines.length > 0) {
    output.push(...agentLines);
  } else {
    output.push("DeepSeekCode edited src/lib.rs and validated the fix.");
  }

  output.push("", diff.value);
  const diffLines = lines
    .slice(diff.index + 1, final.index)
    .filter((line) => /^\-\s+a - b\s*$|^\+\s+a \+ b\s*$/.test(line));
  output.push(...diffLines, "", final.value);

  const passedLines = lines
    .slice(final.index + 1)
    .filter((line) => /\.\.\.\s+ok\b|test result:\s+ok\b/i.test(line))
    .slice(0, 3);
  output.push(...passedLines);
  return output.filter((line, index, all) => !(line === "" && all[index - 1] === ""));
}

function wrapLine(line, width) {
  if (line.length <= width) {
    return [line];
  }
  const chunks = [];
  let remaining = line;
  while (remaining.length > width) {
    let breakAt = remaining.lastIndexOf(" ", width);
    if (breakAt < Math.floor(width * 0.55)) {
      breakAt = width;
    }
    chunks.push(remaining.slice(0, breakAt));
    remaining = `  ${remaining.slice(breakAt).trimStart()}`;
  }
  chunks.push(remaining);
  return chunks;
}

function classify(line) {
  if (line === "") {
    return "muted";
  }
  if (line.startsWith("$")) {
    return "command";
  }
  if (/^\-\s/.test(line)) {
    return "remove";
  }
  if (/^\+\s/.test(line)) {
    return "add";
  }
  if (/FAILED|test failed/i.test(line)) {
    return "fail";
  }
  if (/\bok\b|validated|Fixed|edited/i.test(line)) {
    return "pass";
  }
  return "text";
}

function renderSvg(lines) {
  const width = 1180;
  const paddingX = 36;
  const top = 66;
  const lineHeight = 22;
  const wrapped = [];
  for (const line of lines) {
    for (const part of wrapLine(line, 104)) {
      wrapped.push(part);
    }
  }
  const height = top + wrapped.length * lineHeight + 34;
  const text = wrapped
    .map((line, index) => {
      const y = top + index * lineHeight;
      return `<text x="${paddingX}" y="${y}" class="${classify(line)}">${escapeXml(line)}</text>`;
    })
    .join("\n");

  return `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" role="img" aria-label="DeepSeekCode model-backed coding demo">
  <style>
    .bg { fill: #111820; }
    .bar { fill: #1d2633; }
    .title { fill: #d7dde8; font: 600 16px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
    text { font: 15px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
    .text { fill: #d7dde8; }
    .muted { fill: #718096; }
    .command { fill: #7dd3fc; }
    .remove { fill: #fca5a5; }
    .add { fill: #86efac; }
    .fail { fill: #fb7185; }
    .pass { fill: #86efac; }
  </style>
  <rect class="bg" width="${width}" height="${height}" rx="8"/>
  <rect class="bar" width="${width}" height="44" rx="8"/>
  <circle cx="24" cy="22" r="6" fill="#ff5f58"/>
  <circle cx="44" cy="22" r="6" fill="#ffbd2e"/>
  <circle cx="64" cy="22" r="6" fill="#18c132"/>
  <text x="92" y="28" class="title">DeepSeekCode model-backed coding demo</text>
${text}
</svg>
`;
}

function renderFile(transcriptPath, outputPath) {
  const transcript = fs.readFileSync(transcriptPath, "utf8");
  const svg = renderSvg(collectHighlights(transcript));
  fs.writeFileSync(outputPath, svg, "utf8");
  return { outputPath, bytes: Buffer.byteLength(svg) };
}

function selfTest() {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "deepseek-code-demo-svg-"));
  const transcriptPath = path.join(tmpDir, "transcript.log");
  const svgPath = path.join(tmpDir, "demo.svg");
  const transcript = `DeepSeekCode model-backed coding demo
workspace: /tmp/deepseek-code-model-demo

$ cargo test
running 1 test
test tests::add_returns_sum ... FAILED
test result: FAILED. 0 passed; 1 failed

$ DSCODE_AUTO_APPROVE_WRITES=1 DSCODE_AUTO_APPROVE_SHELL=1 /repo/target/debug/deepseek exec --budget 8 "<prompt>"
Fixed the bug and ran cargo test.

$ git diff -- src/lib.rs
-    a - b
+    a + b

$ cargo test
running 1 test
test tests::add_returns_sum ... ok
test result: ok. 1 passed; 0 failed
`;

  try {
    fs.writeFileSync(transcriptPath, transcript, "utf8");
    renderFile(transcriptPath, svgPath);
    const svg = fs.readFileSync(svgPath, "utf8");
    if (!svg.includes("<svg") || !svg.includes("deepseek exec") || !svg.includes("a + b")) {
      fail("self-test SVG did not include expected demo content");
    }
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

function parseArgs(argv) {
  const args = argv.slice(2);
  let outputPath = "docs/demo/deepseek-code-model-demo.svg";
  let selfTestOnly = false;
  const positional = [];
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--help" || arg === "-h") {
      console.log(usage);
      process.exit(0);
    }
    if (arg === "--self-test") {
      selfTestOnly = true;
      continue;
    }
    if (arg === "--out") {
      outputPath = args[index + 1];
      if (!outputPath) {
        fail("--out requires a path");
      }
      index += 1;
      continue;
    }
    positional.push(arg);
  }
  return { outputPath, selfTestOnly, positional };
}

function main(argv) {
  const { outputPath, selfTestOnly, positional } = parseArgs(argv);
  if (selfTestOnly) {
    selfTest();
    console.log("model-backed demo SVG renderer self-test ok");
    return;
  }
  if (positional.length !== 1) {
    console.error(usage);
    process.exitCode = 2;
    return;
  }
  const result = renderFile(positional[0], outputPath);
  console.log(`model-backed demo SVG updated: ${result.outputPath} (${result.bytes} bytes)`);
}

try {
  main(process.argv);
} catch (error) {
  console.error(`model-backed demo SVG render failed: ${error.message}`);
  process.exit(1);
}
