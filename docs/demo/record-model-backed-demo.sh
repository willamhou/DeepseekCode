#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: docs/demo/record-model-backed-demo.sh [--dry-run] [--cleanup] [--help]

Records a model-backed DeepSeekCode coding loop against a disposable Rust repo:
failing test -> deepseek exec edit -> git diff -> passing cargo test.

Environment:
  DEEPSEEK_API_KEY          Required unless DEEPSEEK_DEMO_ALLOW_OFFLINE=1.
  DEEPSEEK_DEMO_ALLOW_OFFLINE=1
                            Allow offline fallback for local rehearsal only.
  DEEPSEEK_DEMO_BIN         DeepSeekCode binary to run. Defaults to target/debug/deepseek,
                            then PATH deepseek, then builds target/debug/deepseek.
  DEEPSEEK_DEMO_BUDGET      Agent step budget. Defaults to 8.
  DEEPSEEK_DEMO_OUT         Transcript path. Defaults to a timestamped file in docs/demo/.
  DEEPSEEK_DEMO_WORKDIR     Parent directory for the disposable repo.
  DEEPSEEK_DEMO_PROMPT      Override the coding task prompt.

The generated transcript is suitable as source evidence for README GIF/MP4
capture. Review it before committing generated media.
EOF
}

dry_run=0
cleanup=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      dry_run=1
      shift
      ;;
    --cleanup)
      cleanup=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
run_id=$(date +%Y%m%d-%H%M%S)
demo_budget=${DEEPSEEK_DEMO_BUDGET:-8}
demo_prompt=${DEEPSEEK_DEMO_PROMPT:-"Fix the failing Rust test by replacing the subtraction bug in src/lib.rs with addition, then run cargo test and summarize the diff."}
demo_out=${DEEPSEEK_DEMO_OUT:-"$repo_root/docs/demo/deepseek-code-model-demo-$run_id.log"}
work_parent=${DEEPSEEK_DEMO_WORKDIR:-"${TMPDIR:-/tmp}"}
demo_repo="$work_parent/deepseek-code-model-demo-$run_id"

if [[ "$dry_run" -eq 1 ]]; then
  echo "DeepSeekCode model-backed demo dry run"
  echo "repo_root: $repo_root"
  echo "demo_repo: $demo_repo"
  echo "transcript: $demo_out"
  echo "budget: $demo_budget"
  echo "prompt: $demo_prompt"
  echo "status: dry-run only; no API call, repository creation, or transcript write"
  exit 0
fi

if [[ "${DEEPSEEK_DEMO_ALLOW_OFFLINE:-0}" != "1" && -z "${DEEPSEEK_API_KEY:-}" ]]; then
  echo "DEEPSEEK_API_KEY is required for model-backed README demo evidence." >&2
  echo "Set DEEPSEEK_DEMO_ALLOW_OFFLINE=1 only for local rehearsal; do not publish that as model-backed evidence." >&2
  exit 1
fi

if [[ -n "${DEEPSEEK_DEMO_BIN:-}" ]]; then
  deepseek_bin=$DEEPSEEK_DEMO_BIN
elif [[ -x "$repo_root/target/debug/deepseek" ]]; then
  deepseek_bin="$repo_root/target/debug/deepseek"
elif command -v deepseek >/dev/null 2>&1; then
  deepseek_bin=$(command -v deepseek)
else
  echo "target/debug/deepseek not found; building debug binary" >&2
  cargo build --manifest-path "$repo_root/Cargo.toml" --bin deepseek
  deepseek_bin="$repo_root/target/debug/deepseek"
fi

if [[ ! -x "$deepseek_bin" ]]; then
  echo "DeepSeekCode binary is not executable: $deepseek_bin" >&2
  exit 1
fi

mkdir -p "$demo_repo/src"
mkdir -p "$(dirname -- "$demo_out")"

cat > "$demo_repo/Cargo.toml" <<'EOF'
[package]
name = "deepseek-code-demo-fixture"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
EOF

cat > "$demo_repo/src/lib.rs" <<'EOF'
pub fn add(a: i32, b: i32) -> i32 {
    a - b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_returns_sum() {
        assert_eq!(add(2, 3), 5);
    }
}
EOF

git -C "$demo_repo" init -q
git -C "$demo_repo" config user.email "demo@deepseekcode.local"
git -C "$demo_repo" config user.name "DeepSeekCode Demo"
git -C "$demo_repo" add Cargo.toml src/lib.rs
git -C "$demo_repo" commit -q -m "Create failing demo fixture"

run_session() {
  cd "$demo_repo"
  echo "DeepSeekCode model-backed coding demo"
  echo "workspace: $demo_repo"
  echo
  echo "$ cargo test"
  cargo test || true
  echo
  echo "$ DSCODE_AUTO_APPROVE_WRITES=1 DSCODE_AUTO_APPROVE_SHELL=1 $deepseek_bin exec --budget $demo_budget \"<prompt>\""
  DSCODE_AUTO_APPROVE_WRITES=1 \
    DSCODE_AUTO_APPROVE_SHELL=1 \
    "$deepseek_bin" exec --budget "$demo_budget" "$demo_prompt"
  echo
  echo "$ git diff -- src/lib.rs"
  git diff -- src/lib.rs
  echo
  echo "$ cargo test"
  cargo test
}

set +e
run_session 2>&1 | tee "$demo_out"
session_status=${PIPESTATUS[0]}
set -e

echo
echo "transcript: $demo_out"
echo "demo repo: $demo_repo"

if [[ "$cleanup" -eq 1 ]]; then
  rm -rf "$demo_repo"
  echo "demo repo removed"
fi

exit "$session_status"
