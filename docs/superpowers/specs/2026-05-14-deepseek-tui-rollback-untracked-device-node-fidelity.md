# DeepSeek-TUI Rollback Untracked Device Node Fidelity

Status: implemented

## Gap

Phase F rollback fidelity already captured untracked regular files, empty
directories, Unix directory modes, FIFOs, sockets, and symlinks. The remaining
Unix special-file gap still called out device nodes, which meant snapshots could
not even record character/block device metadata for worktrees that contain
fixture device nodes.

## Implementation

- Added `untracked_device_nodes` manifest records with path, kind
  (`char`/`block`), major, minor, and mode.
- Captured untracked Unix character and block device nodes while preserving the
  existing ignore, git-internal, rollback-storage, and tracked-file filters.
- Included device-node paths in directory-mode parent metadata collection and
  applied restore changed-file reporting.
- Added best-effort Unix restore through `mknod`, followed by mode restoration.
  Restore fails clearly if the current user/OS lacks permission to recreate the
  node.
- Kept non-Unix behavior as an empty capture set with an explicit unsupported
  restore error.
- Updated runtime docs and the DeepSeek-TUI parity plan.

## Verification

- `cargo test snapshot_manifest_round_trips_untracked_device_nodes --lib`
- `cargo test rollback --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

This closes manifest-level Unix device-node fidelity and best-effort restore.
It does not prove privileged live device-node recreation in CI, and Windows
symlink recreation remains a platform-specific rollback boundary.
