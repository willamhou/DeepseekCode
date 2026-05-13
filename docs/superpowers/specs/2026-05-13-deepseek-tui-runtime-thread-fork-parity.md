# DeepSeek-TUI Runtime Thread Fork Parity

Date: 2026-05-13

Status: completed

## Gap

The durable runtime had sessions, threads, turns, items, compaction, task
queues, and ACP session loading, but it did not expose a first-class fork for
branching a conversation context. That left a Phase B resume/fork gap versus
workbench runtimes that let clients branch from existing context without
rewriting the source thread.

## Spec

1. Add `RuntimeStore::fork_thread(source_thread_id, title)` for durable thread
   branching.
2. Copy source turns and items into a new active thread in the same session.
3. Remap copied turn and item ids so the fork is independently mutable.
4. Preserve transcript content/status ordering, but do not copy source usage
   records or historical events.
5. Append `thread_created` and `thread_forked` events on the fork thread, and
   update the parent session's active thread/count.
6. Expose `POST /v1/threads/{id}/fork` and advertise `thread_fork` in
   `/runtime`.

## Implementation

- `ThreadForkRecord` records the source thread, new thread, copied counts, and
  fork audit event.
- `thread_fork_to_json` renders `deepseek.runtime.thread_fork.v1` payloads.
- HTTP runtime accepts an optional `{"title":"..."}` body for fork titles.

## Verification

- `/home/willamhou/.cargo/bin/cargo test fork_thread_copies_turns_and_items_with_new_ids --lib`
- `/home/willamhou/.cargo/bin/cargo test thread_fork_endpoint_copies_runtime_thread_context --lib`
- `/home/willamhou/.cargo/bin/cargo test runtime --lib`
- `/home/willamhou/.cargo/bin/cargo test serve --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `git diff --check`
