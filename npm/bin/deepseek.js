#!/usr/bin/env node
"use strict";

const { existsSync } = require("node:fs");
const { dirname, join } = require("node:path");
const { spawnSync } = require("node:child_process");

const TARGETS = {
  "darwin:arm64": {
    packageSuffix: "macos-arm64",
    triple: "aarch64-apple-darwin",
  },
  "darwin:x64": {
    packageSuffix: "macos-x64",
    triple: "x86_64-apple-darwin",
  },
  "linux:arm64": {
    packageSuffix: null,
    triple: "aarch64-unknown-linux-gnu",
  },
  "linux:x64": {
    packageSuffix: "linux-x64",
    triple: "x86_64-unknown-linux-gnu",
  },
  "win32:arm64": {
    packageSuffix: null,
    triple: "aarch64-pc-windows-msvc",
  },
  "win32:x64": {
    packageSuffix: "windows-x64",
    triple: "x86_64-pc-windows-msvc",
  },
};

function platformTarget() {
  return TARGETS[`${process.platform}:${process.arch}`] || null;
}

function optionalPackageBinary(target, executable) {
  if (!target || !target.packageSuffix) {
    return null;
  }
  const packageName = `@deepseek-code/cli-${target.packageSuffix}`;
  try {
    const packageJson = require.resolve(`${packageName}/package.json`, {
      paths: [__dirname],
    });
    return join(dirname(packageJson), "bin", executable);
  } catch {
    return null;
  }
}

function candidateBinaries() {
  const executable = process.platform === "win32" ? "deepseek.exe" : "deepseek";
  const target = platformTarget();
  const candidates = [];
  if (process.env.DEEPSEEK_BINARY) {
    candidates.push(process.env.DEEPSEEK_BINARY);
  }
  const optionalBinary = optionalPackageBinary(target, executable);
  if (optionalBinary) {
    candidates.push(optionalBinary);
  }
  if (target && target.triple) {
    candidates.push(join(__dirname, target.triple, executable));
  }
  if (target && target.packageSuffix) {
    candidates.push(
      join(__dirname, "..", "platforms", target.packageSuffix, "bin", executable),
    );
  }
  candidates.push(join(__dirname, "..", "..", "target", "release", executable));
  return candidates;
}

function installHint() {
  const target = platformTarget();
  if (target && target.packageSuffix) {
    return [
      `Install or publish the optional platform package @deepseek-code/cli-${target.packageSuffix},`,
      "or set DEEPSEEK_BINARY to an existing deepseek executable.",
    ].join("\n");
  }
  return [
    "Set DEEPSEEK_BINARY to an existing deepseek executable,",
    "or publish a platform binary under npm/bin/<target-triple>/deepseek.",
  ].join("\n");
}

const binary = candidateBinaries().find((candidate) => existsSync(candidate));

if (!binary) {
  console.error(
    [
      "deepseek npm wrapper could not find a packaged binary.",
      installHint(),
    ].join("\n"),
  );
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(result.error.message);
  process.exit(1);
}
process.exit(result.status === null ? 1 : result.status);
