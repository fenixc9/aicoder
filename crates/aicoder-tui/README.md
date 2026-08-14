# aicoder-tui

The interactive frontend consumes ordered `aicoder-core` events through one
application channel. Keyboard input, agent events, approval requests, and turn
completion all reduce into `App` state before rendering, so callbacks never
mutate terminal state from worker threads.

Run from the repository root:

~~~bash
cargo run -p aicoder-tui -- --workspace . --model "$OPENAI_MODEL"
~~~

Open a known session directly with its full ID:

~~~bash
cargo run -p aicoder-tui -- --workspace . --session <SESSION_ID>
~~~

Environment variables follow `ChatClient::from_env`: set an API key such as
`OPENAI_API_KEY` or `DEEPSEEK_API_KEY`, and optionally `OPENAI_API_BASE`.
The TUI loads `.env` from the current directory search path, then falls back to
`crates/aicoder-tui/.env` and the existing `crates/aicoder-cli/.env`.
Sessions and logs live under `AICODER_HOME` when set, otherwise `~/.aicoder`.

Type `/` to open the command list and keep typing to filter it. Use Up/Down to
select, Tab to complete, Enter to run, and Esc to dismiss the list. The command
registry currently provides `/exit`, which closes the TUI without starting an
agent turn.

The first release intentionally permits one active turn. This keeps session
writes ordered and makes cancellation deterministic. The UI disables prompt
submission and session switching until the current turn completes or aborts.
