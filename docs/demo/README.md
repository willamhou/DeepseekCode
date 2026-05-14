# Demo Assets

`deepseek-code-tui.svg` is a deterministic README snapshot generated from:

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
