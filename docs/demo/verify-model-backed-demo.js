#!/usr/bin/env node

const fs = require("fs");
const os = require("os");
const path = require("path");

const usage = `Usage: docs/demo/verify-model-backed-demo.js [--self-test] <transcript.log>

Verifies that a model-backed README demo transcript shows:
  failing cargo test -> deepseek exec -> git diff -> passing cargo test.

The verifier also rejects obvious API tokens, redaction markers, and offline
rehearsal transcripts so reviewed README media is backed by real model output.`;

function fail(message) {
  throw new Error(message);
}

function indexOfRegex(text, regex, start = 0) {
  const flags = regex.flags.includes("g") ? regex.flags : `${regex.flags}g`;
  const globalRegex = new RegExp(regex.source, flags);
  globalRegex.lastIndex = start;
  const match = globalRegex.exec(text);
  return match ? match.index : -1;
}

function assertOrdered(name, earlierName, earlierIndex, laterIndex) {
  if (laterIndex < 0) {
    fail(`missing required marker: ${name}`);
  }
  if (laterIndex <= earlierIndex) {
    fail(`${name} must appear after ${earlierName}`);
  }
}

function validateTranscript(text, label = "transcript") {
  if (!text.trim()) {
    fail(`${label} is empty`);
  }

  const secretMatch = text.match(/\bsk-[A-Za-z0-9_-]{20,}\b/);
  if (secretMatch) {
    fail(`found API-key-shaped token in ${label}`);
  }

  const redactionMarker = text.match(
    /<(?:DEEPSEEK_API_KEY|OPENAI_API_KEY|ANTHROPIC_API_KEY|DEEPSEEK_DEMO_KEY_FILE):redacted>/,
  );
  if (redactionMarker) {
    fail(`found redaction marker in ${label}; review source output before publishing media`);
  }

  if (/DEEPSEEK_DEMO_ALLOW_OFFLINE=1|allow_offline|offline fallback|offline rehearsal/i.test(text)) {
    fail(`${label} appears to be an offline rehearsal, not model-backed evidence`);
  }

  if (!text.includes("DeepSeekCode model-backed coding demo")) {
    fail("missing DeepSeekCode model-backed demo header");
  }

  const cargoFailurePattern = /test result:\s+FAILED|\.\.\.\s+FAILED\b|\bfailures:\b|error:\s+test failed/i;
  const initialTestIndex = text.indexOf("$ cargo test");
  if (initialTestIndex < 0) {
    fail("missing initial cargo test command");
  }

  const execIndex = indexOfRegex(text, /\bdeepseek(?:\.exe)?\s+exec\b/i, initialTestIndex);
  assertOrdered("deepseek exec command", "initial cargo test", initialTestIndex, execIndex);

  const initialTestSegment = text.slice(initialTestIndex, execIndex);
  if (!cargoFailurePattern.test(initialTestSegment)) {
    fail("initial cargo test segment does not show a failing test");
  }

  const diffIndex = text.indexOf("$ git diff -- src/lib.rs", execIndex);
  assertOrdered("git diff command", "deepseek exec command", execIndex, diffIndex);

  const finalTestIndex = text.indexOf("$ cargo test", diffIndex);
  assertOrdered("final cargo test command", "git diff command", diffIndex, finalTestIndex);

  const diffSegment = text.slice(diffIndex, finalTestIndex);
  if (!/^\-\s+a - b\s*$/m.test(diffSegment)) {
    fail("diff segment does not remove the subtraction bug");
  }
  if (!/^\+\s+a \+ b\s*$/m.test(diffSegment)) {
    fail("diff segment does not add the addition fix");
  }

  const finalTestSegment = text.slice(finalTestIndex);
  if (!/test result:\s+ok\b/i.test(finalTestSegment)) {
    fail("final cargo test segment does not show passing tests");
  }
  if (cargoFailurePattern.test(finalTestSegment)) {
    fail("final cargo test segment still contains a failure");
  }

  return {
    label,
    bytes: Buffer.byteLength(text),
    initialTestIndex,
    execIndex,
    diffIndex,
    finalTestIndex,
  };
}

function runSelfTest() {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "deepseek-code-demo-verify-"));
  const transcriptPath = path.join(tmpDir, "transcript.log");
  const transcript = `DeepSeekCode model-backed coding demo
workspace: /tmp/deepseek-code-model-demo

$ cargo test
running 1 test
test tests::add_returns_sum ... FAILED
test result: FAILED. 0 passed; 1 failed

$ DSCODE_AUTO_APPROVE_WRITES=1 DSCODE_AUTO_APPROVE_SHELL=1 /repo/target/debug/deepseek exec --budget 8 "<prompt>"
Fixed the bug and ran the requested validation.

$ git diff -- src/lib.rs
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,5 +1,5 @@
 pub fn add(a: i32, b: i32) -> i32 {
-    a - b
+    a + b
 }

$ cargo test
running 1 test
test tests::add_returns_sum ... ok
test result: ok. 1 passed; 0 failed
`;

  function expectReject(mutatedTranscript, expectedMessage) {
    try {
      validateTranscript(mutatedTranscript, "self-test mutated transcript");
    } catch (error) {
      if (error.message.includes(expectedMessage)) {
        return;
      }
      throw new Error(`expected rejection containing "${expectedMessage}", got "${error.message}"`);
    }
    throw new Error(`expected rejection containing "${expectedMessage}"`);
  }

  try {
    fs.writeFileSync(transcriptPath, transcript, "utf8");
    validateTranscript(fs.readFileSync(transcriptPath, "utf8"), transcriptPath);
    expectReject(`${transcript}\n${"sk-" + "x".repeat(24)}\n`, "API-key-shaped token");
    expectReject(`${transcript}\nDEEPSEEK_DEMO_ALLOW_OFFLINE=1\n`, "offline rehearsal");
    expectReject(`${transcript}\n<DEEPSEEK_API_KEY:redacted>\n`, "redaction marker");
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

function main(argv) {
  const args = argv.slice(2);
  if (args.includes("--help") || args.includes("-h")) {
    console.log(usage);
    return;
  }
  if (args.includes("--self-test")) {
    runSelfTest();
    console.log("model-backed demo verifier self-test ok");
    return;
  }

  if (args.length !== 1) {
    console.error(usage);
    process.exitCode = 2;
    return;
  }

  const transcriptPath = args[0];
  const text = fs.readFileSync(transcriptPath, "utf8");
  const result = validateTranscript(text, transcriptPath);
  console.log(`model-backed demo transcript ok: ${result.label} (${result.bytes} bytes)`);
}

if (require.main === module) {
  try {
    main(process.argv);
  } catch (error) {
    console.error(`model-backed demo transcript verification failed: ${error.message}`);
    process.exit(1);
  }
}

module.exports = {
  validateTranscript,
};
