# DeepSeek-TUI Parity: TUI Clear Command

## Context

DeepSeek-TUI exposes `/clear` as a core command that clears the current
conversation history, queued messages, API messages, tool state, and session
telemetry. DeepSeekCode's TUI is backed by durable runtime threads, so clearing
only the visible transcript would be unstable: the next runtime refresh would
reload the old durable items.

## Goals

- Add `clear` / `/clear` command palette and composer support.
- Add `clear help` / `/clear help` explaining durable-runtime behavior.
- Preserve existing durable thread history instead of deleting runtime files.
- Create a new empty active thread in the selected durable session.
- Switch the TUI to the new thread so the visible transcript and next composer
  turn start from a fresh context.
- Drop transient queued follow-up messages and composer/detail state after the
  reset.
- Reject clear in HTTP-runtime TUI mode until remote thread creation can carry
  the same model/mode metadata.

## Design

`TuiAction::ClearConversation { session_id, previous_thread_id }` captures the
selected durable session and the current active thread. The local file-backed
handler loads the previous thread to preserve workspace/model/mode metadata,
creates `New conversation` through `RuntimeStore::create_thread_for_session`,
refreshes the TUI snapshot from the store, clears transient UI state, and then
selects the new thread.

This maps DeepSeek-TUI's in-memory reset onto DeepSeekCode's durable model:
users get a fresh active context, while older transcript history remains
available in the thread navigator.

## Acceptance

- `/clear` queues a clear action for the selected durable session.
- `/clear help` renders reset behavior and the current target in the detail
  panel.
- Local file-backed TUI creates a new active thread, preserves prior thread
  history, and switches the UI to the empty new thread.
- Queued follow-up/composer/detail transient state is dropped after reset.
- HTTP-runtime TUI rejects clear as local-only for now.
- Tests cover command routing, local clear behavior, and HTTP rejection.
