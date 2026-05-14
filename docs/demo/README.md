# Demo Assets

`deepseek-code-tui-demo.svg` is the animated README demo generated from the
deterministic TUI snapshot:

```bash
svg-term --command "bash -lc 'target/debug/deepseek tui --demo --once | sed -e \"s/^\\\"//\" -e \"s/\\\"$//\" | while IFS= read -r line; do printf \"%s\\n\" \"\$line\"; sleep 0.08; done; sleep 1.5'" \
  --out docs/demo/deepseek-code-tui-demo.svg \
  --width 122 \
  --height 36 \
  --window \
  --no-cursor
```

`deepseek-code-tui.svg` is the static snapshot generated from the same
deterministic TUI output:

```bash
svg-term --command "bash -lc 'target/debug/deepseek tui --demo --once | sed -e \"s/^\\\"//\" -e \"s/\\\"$//\"; sleep 1'" \
  --out docs/demo/deepseek-code-tui.svg \
  --width 122 \
  --height 36 \
  --window \
  --no-cursor \
  --at 1000
```

For a launch-quality README, add a short GIF or MP4 that shows the real coding
loop: open the TUI, submit a request, apply an edit, run tests, and inspect the
diff.

## Model-Backed Demo Capture

Use `record-model-backed-demo.sh` to capture real model-backed CLI evidence
against a disposable Rust repository. The script creates a small failing crate,
shows the initial failing `cargo test`, runs `deepseek exec` with write/shell
approvals limited to that disposable repository, then records the final diff
and passing test output.

Dry-run the capture plan without requiring an API key:

```bash
docs/demo/record-model-backed-demo.sh --dry-run
```

Record a real model-backed transcript:

```bash
DEEPSEEK_API_KEY=... docs/demo/record-model-backed-demo.sh
```

The default output is a timestamped `docs/demo/deepseek-code-model-demo-*.log`
transcript. Convert a reviewed successful run into the GIF/MP4 or SVG asset
linked from the README; do not publish runs created with
`DEEPSEEK_DEMO_ALLOW_OFFLINE=1` as model-backed evidence.
