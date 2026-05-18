#!/usr/bin/env node
"use strict";

const { existsSync, statSync } = require("node:fs");
const { join, resolve } = require("node:path");
const { spawnSync } = require("node:child_process");

const repoRoot = resolve(__dirname, "..", "..");
const wrapper = join(repoRoot, "npm", "bin", "deepseek.js");
const binary = process.env.DEEPSEEK_BINARY || join(repoRoot, "target", "debug", "deepseek");

function fail(message) {
  console.error(message);
  process.exit(1);
}

if (!existsSync(binary)) {
  fail(`DeepSeekCode binary is missing: ${binary}\nRun cargo build --bin deepseek or set DEEPSEEK_BINARY.`);
}

if (!existsSync(wrapper)) {
  fail(`npm wrapper is missing: ${wrapper}`);
}

if ((statSync(wrapper).mode & 0o111) === 0) {
  fail(`npm wrapper is not executable: ${wrapper}`);
}

const result = spawnSync(
  binary,
  ["tui", "--entrypoint-smoke", "--smoke-bin", wrapper],
  {
    cwd: repoRoot,
    encoding: "utf8",
    env: { ...process.env, DEEPSEEK_BINARY: binary },
  },
);

if (result.stdout) {
  process.stdout.write(result.stdout);
}
if (result.stderr) {
  process.stderr.write(result.stderr);
}
if (result.error && result.status === null) {
  fail(result.error.message);
}
if (result.status !== 0) {
  process.exit(result.status || 1);
}

const lines = result.stdout.trim().split(/\r?\n/).filter(Boolean);
const rawReport = lines[lines.length - 1] || "";
let report;
try {
  report = JSON.parse(rawReport);
} catch (error) {
  fail(`failed to parse TUI entrypoint smoke JSON: ${error.message}`);
}

const failures = [];
if (report.schema !== "deepseek.tui.entrypoint_smoke.v1") {
  failures.push(`unexpected schema ${report.schema}`);
}
for (const key of ["ok", "entered_alternate_screen", "left_alternate_screen", "rendered_tui"]) {
  if (report[key] !== true) {
    failures.push(`${key} was not true`);
  }
}
if (report.bin !== wrapper) {
  failures.push(`smoked ${report.bin}, expected ${wrapper}`);
}
if (failures.length > 0) {
  fail(`npm wrapper TUI entrypoint smoke failed:\n${failures.join("\n")}`);
}

console.log("npm wrapper TUI entrypoint smoke ok");
