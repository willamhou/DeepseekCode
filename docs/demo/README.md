# Demo Assets

`deepseek-code-tui-demo.svg` is the animated README demo generated from the
deterministic TUI snapshot. `deepseek-code-tui.svg` is the static fallback from
the same snapshot.

```bash
docs/demo/record-readme-demo.sh
```

The recorder defaults to `target/debug/deepseek`, then PATH `deepseek`, then
builds the debug binary. It requires `svg-term` and fails if the animated SVG is
missing keyframes.

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
printf '%s\n' '<deepseek-api-key>' > /tmp/deepseek-demo.key
chmod 600 /tmp/deepseek-demo.key
DEEPSEEK_DEMO_KEY_FILE=/tmp/deepseek-demo.key docs/demo/record-model-backed-demo.sh
```

The default output is a timestamped `docs/demo/deepseek-code-model-demo-*.log`
transcript. Convert a reviewed successful run into the GIF/MP4 or SVG asset
linked from the README; do not publish runs created with
`DEEPSEEK_DEMO_ALLOW_OFFLINE=1` as model-backed evidence.

`DEEPSEEK_DEMO_KEY_FILE` must point outside this repository so API keys cannot
be accidentally committed. `--api-key-stdin` is also supported when piping from
a local secret manager.
