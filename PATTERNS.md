# PATTERNS.md — xlightcli code patterns

> **Mandatory** patterns when writing code in this repo. Snippets show the target shape (not real code yet);
> once implemented, update the snippet to match the real code or point to the corresponding file.
> General rules: [`AGENTS.md`](AGENTS.md). Invariants: [`docs/PLAN.md`](docs/PLAN.md) §2.

Table of contents:
1. Workspace & lint · 2. Error handling · 3. Async, cancellation, channel · 4. Credential ·
5. Provider adapter · 6. Canonical event stream · 7. Tool · 8. Process spawning · 9. Output spooling ·
10. Persistence · 11. Config · 12. Commands · 13. Testing · 14. Logging · 15. Anti-patterns

---

## 1. Workspace & lint

- Every `.rs` file starts with the SPDX header (D-028). Files ported from OpenCodex add a provenance line:

```rust
// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ <commit-sha>: src/oauth/pkce.ts
// See THIRD_PARTY.md.
```

- Every dependency declares its version in `[workspace.dependencies]`; sub-crates use `foo = { workspace = true }`.
- Root `Cargo.toml`: `[workspace.package] license = "GPL-3.0-only"`; sub-crates use `license.workspace = true`.
- Shared lints live in `[workspace.lints]`, sub-crates have `[lints] workspace = true`.

```toml
# Cargo.toml (root)
[workspace.lints.rust]
unsafe_code = "forbid"
missing_debug_implementations = "warn"

[workspace.lints.clippy]
unwrap_used = "warn"
expect_used = "warn"
print_stdout = "warn"
print_stderr = "warn"
dbg_macro = "warn"
```

```toml
# clippy.toml
disallowed-methods = [
  { path = "std::process::Command::new", reason = "spawn only via tools::launcher::ProcessLauncher (INV-1)" },
  { path = "tokio::process::Command::new", reason = "spawn only via tools::launcher::ProcessLauncher (INV-1)" },
  { path = "tokio::sync::mpsc::unbounded_channel", reason = "use bounded channels for backpressure (INV-6)" },
]
```

The only place allowed `#[allow(clippy::disallowed_methods)]` for `Command::new` is `crates/tools/src/launcher.rs`.

## 2. Error handling

- Library crate: `thiserror` error enum, categorized (the caller needs to decide retry/refresh/report to user).
- `anyhow` only in `app` and `xtask`.
- Never put a secret or a raw response body into an error message.

```rust
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("no credential for {provider}")]
    NotLoggedIn { provider: ProviderId },
    #[error("refresh rejected by upstream (re-login required)")]
    RefreshRejected,
    #[error("secret store unavailable: {0}")]
    StoreUnavailable(String),
    #[error("oauth flow failed: {0}")]
    OAuth(String),
}
```

Map wire errors → `ProviderError` **exactly once** at the transport boundary (`fn map_status(status, headers, body_excerpt)`), not scattered around.

## 3. Async, cancellation, channel

- Tokio multi-thread. Never block inside async: large file IO uses `tokio::fs` or `spawn_blocking`; SQLite only on the writer thread.
- Every long-running task takes a `CancellationToken` (`tokio_util::sync`). Child token for a child agent: `parent.child_token()`.
- Channels are always bounded; pick a size deliberately and comment it:

```rust
// UI deltas are coalesced by the receiver; 256 keeps ~1 frame of backlog per agent.
let (ui_tx, ui_rx) = tokio::sync::mpsc::channel::<UiEvent>(256);
```

- UI deltas: if the channel is full, **coalesce** the text delta instead of blocking the provider stream indefinitely; important events (`Completed`, tool, permission) must never be dropped.
- Trait used via `dyn` ⇒ `#[async_trait]` (D-006). Trait only used generically ⇒ native `async fn`.
- `select!` with cancel always puts the cancel branch first (`biased;`) so cancellation reacts fast.

```rust
tokio::select! {
    biased;
    _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
    next = stream.next() => { /* ... */ }
}
```

## 4. Credential (INV-4)

- Secrets only ever exist inside the `auth` crate, wrapped in `secrecy::SecretString`, zeroized on drop.
- Outside `auth` there is only `CredentialHandle`: never exposes the secret, `Debug` only prints metadata.
- The transport inserts the header via the handle, never holds the token as a `String`:

```rust
let mut headers = HeaderMap::new();
cred.authorize(&mut headers).await?;
let resp = self.http.post(url).headers(headers).json(&body).send().await?;
if resp.status() == StatusCode::UNAUTHORIZED {
    cred.on_unauthorized().await?;          // single-flight refresh
    // retry exactly once, then surface ProviderError::Auth
}
```

- Single-flight refresh: one `tokio::sync::Mutex` (or `OnceCell` keyed by generation) per account; tasks arriving later wait for the result of the refresh already in progress instead of sending a second request.
- Never `impl Serialize` for a type that holds a secret outside the `store` module.

## 5. Provider adapter

Layout of a `provider-<x>` crate:

```text
src/
├── lib.rs                 pub struct XProvider; impl Provider
├── auth.rs                impl AuthAdapter (login flows, discovery, import, refresh)
├── wire/                  pub(crate) wire types + translator
│   ├── request.rs         TurnRequest  → wire body
│   ├── response.rs        wire SSE     → AgentEvent (state machine)
│   └── mod.rs
├── transport_<id>.rs      impl TransportAdapter (endpoint, headers, gating)
└── features/              impl ProviderFeaturePack + commands
tests/
├── fixtures/*.sse         already redacted
└── wire_snapshots.rs      insta snapshots
```

Rules:

- Wire types are `pub(crate)`; no wire type ever appears in a `pub` signature.
- The translator is a **pure state machine** (no IO) so it can be tested with fixtures: `feed(&mut self, sse_event) -> Vec<AgentEvent>` and `finish(self) -> Result<AgentEvent::Completed, ProviderError>`.
- An unknown/missing field that affects semantics ⇒ `ProviderError::ProtocolMismatch`, never guess. A harmless unknown field ⇒ skip it + `tracing::debug!`.
- Continuity data (thinking signature, encrypted reasoning) ⇒ `OpaqueBlob { provider, transport, data }`. When building a request: only replay a blob whose `(provider, transport)` matches.
- Experimental transports sit behind `#[cfg(feature = "...")]` **and** a runtime gate check:

```rust
#[cfg(feature = "claude-subscription")]
impl TransportAdapter for ClaudeSubscriptionTransport {
    fn stability(&self) -> Stability { Stability::Experimental }
    async fn stream(&self, req: TurnRequest, cred: CredentialHandle, cancel: CancellationToken)
        -> Result<EventStream, ProviderError>
    {
        self.gate.ensure_enabled()?;   // experimental opt-in + kill switch → TransportDisabled
        // ...
    }
}
```

- Every endpoint/header/client-id constant lives in one `consts.rs` module of the adapter, with a comment pointing to `docs/providers/<x>.md` and the date it was verified.

## 6. Canonical event stream

The transport returns a pull-based stream (D-007) for natural backpressure:

```rust
async fn stream(&self, req: TurnRequest, cred: CredentialHandle, cancel: CancellationToken)
    -> Result<EventStream, ProviderError>
{
    let resp = self.send(req, &cred).await?;
    let mut sse = provider::sse::parse(resp.bytes_stream());
    let mut tr = wire::response::Translator::new(self.id());
    Ok(Box::pin(async_stream::try_stream! {
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(ProviderError::Cancelled)?,
                ev = sse.next() => match ev {
                    Some(ev) => for out in tr.feed(ev?)? { yield out; },
                    None => break,
                },
            }
        }
        yield tr.finish()?;   // AgentEvent::Completed { message, stop, usage }
    }))
}
```

Stream contract: exactly one `TurnStarted` at the start and exactly one `Completed` at the end on success;
`Completed.message` is the complete assistant message (the runtime does not reassemble it from deltas).

## 7. Tool

```rust
pub struct ReadFile { def: ToolDefinition }

#[async_trait]
impl Tool for ReadFile {
    fn definition(&self) -> &ToolDefinition { &self.def }
    fn effect(&self, _input: &Value) -> ToolEffect { ToolEffect::ReadOnly }

    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: ReadFileArgs = serde_json::from_value(input).map_err(ToolError::invalid_input)?;
        let path = ctx.workspace.resolve(&args.path)?;          // block paths escaping the workspace
        ctx.permissions.check(ToolAction::Read(&path)).await?;  // may open a dialog
        // stream the file into ctx.spool if large; don't read_to_string an entire huge file
        ...
    }
}
```

- Input is parsed with a `serde` struct that has `#[serde(deny_unknown_fields)]`; the JSON schema for the model is generated from the same struct (`schemars`).
- Paths always go through `workspace.resolve()` (canonicalize, block `..`/symlink escaping the root).
- Permission check happens **before** the side effect, inside the tool, not just in the executor.
- A tool never receives credentials and never spawns a process itself (uses `ctx.launcher`).

## 8. Process spawning (INV-1)

```rust
let child = ctx.launcher.spawn(SpawnSpec {
    purpose: SpawnPurpose::ShellTool,
    program: "bash".into(),
    args: vec!["-lc".into(), command],
    cwd: ctx.workspace.root().to_owned(),
    env: EnvPolicy::Scrubbed { passthrough: ctx.config.env_passthrough.clone() },
    timeout: Some(args.timeout.unwrap_or(DEFAULT_SHELL_TIMEOUT)),
    cancel: ctx.cancel.clone(),
}).await?;
let output = child.pipe_into(&ctx.spool).await?;
```

- `ProcessLauncher` creates its own process group, killing the whole group on timeout/cancel.
- `SpawnPurpose::{Mcp, Hook, Git}` with a program basename on the provider-CLI blocklist ⇒ `SpawnError::ForbiddenProgram`.
- `EnvPolicy::Scrubbed` is the default; there is no "inherit the whole environment" API for MCP/hooks.

## 9. Output spooling (INV-7)

```rust
let spool = OutputSpool::create(&artifacts, session_id, call_id, SpoolLimits {
    head_bytes: 8 * 1024,
    tail_bytes: 32 * 1024,
})?;
// writer: every byte → file; head keeps the first N bytes; tail is a ring buffer
let summary: SpooledOutput = spool.finish().await?;
// summary = { head, tail, total_bytes, total_lines, truncated, artifact: ArtifactRef }
```

The model receives `summary` (not the full output). The TUI shows the tail + a link to open the artifact.

## 10. Persistence

- Only the **writer thread** holds the write connection; other code sends a `StorageCmd` over a bounded channel and (if needed) receives an ack via `oneshot`.
- Events are appended in batches (N events or 50 ms). `TextDelta` is coalesced by chunk before writing.
- The reader uses its own read-only connection (WAL allows concurrent reads).
- Migrations only add (`NNNN_description.sql`), never edit an already-merged migration.
- The JSON payload in the DB is the canonical `protocol` type (serde), **not** the provider's wire JSON.
- No column ever holds a secret; `accounts.keyring_ref` is just a keyring lookup key.

## 11. Config

- Each layer deserializes into a `PartialConfig` (every field `Option`), layers are merged in order, then `resolve()` produces the full `Config` with defaults.
- Merge: tables deep-merge; scalars are overwritten; arrays are replaced. Record the source of each key (`Origin`) for `config show --origin`.
- Sensitive keys (hooks, MCP stdio, exec allow rules, permission mode > ask, `[experimental]`, env passthrough) are ignored from the project layer when the workspace isn't trusted — handled in `config::trust`, with tests.
- `[experimental]` is only read from the global layer.

## 12. Commands

```rust
CommandDefinition {
    id: CommandId::new("claude", "insights"),
    alias: "insights",
    mode: CapabilityMode::Compatible,
    requires_transport: None,
    summary: "Analyze session history and suggest workflow improvements",
}
```

- Resolution: core first, then the FeaturePack of the active provider; `/provider:alias` is always explicit.
- An unavailable command ⇒ `CommandResult::Unavailable { reason }` — never fake a result.
- `CommandContext` never holds a credential; to run a secondary model turn, call the runtime's API.

## 13. Testing

| Area | Pattern |
|------|---------|
| Wire translator | Fixture `tests/fixtures/<scenario>.sse` → `Translator` → `insta::assert_yaml_snapshot!(events)`. Each scenario: text, reasoning, tool call (single and parallel), error mid-stream, max tokens. |
| Request builder | Sample `TurnRequest` → wire JSON → snapshot. |
| Transport | `wiremock`: 200 SSE, 401→refresh→200, 401→refresh fail, 429 retry-after, stream cut off, unknown schema. |
| Runtime | `provider::testing::MockProvider::scripted([...events])`; no network, deterministic. |
| Concurrency | `tokio::test(start_paused = true)` for timeout/backoff; test single-flight with N concurrent tasks. |
| Live | `#[ignore]` + `#[cfg_attr(...)]`/env `XLIGHTCLI_LIVE_<PROVIDER>=1`; test name starts with `live_`. |
| Secret leak | Use the sentinel `"XLC-SENTINEL-SECRET"` as the token; assert it never appears in captured logs, the DB file, artifacts, or a child's env. |

Fixtures must go through `cargo xtask redact-fixture` before committing.

## 14. Logging

- `tracing` with spans keyed by `session_id`, `agent_id`, `turn`, `transport` (never a secret as a span field).
- Levels: `error` = needs user action; `warn` = degraded; `info` = lifecycle; `debug` = protocol detail (already redacted); `trace` = never used for bodies.
- Never log the full request/response body; only a redacted excerpt, length-limited.
- Log file at `$XDG_STATE_HOME/xlightcli/logs/`; the TUI never prints logs to stdout.

## 15. Anti-patterns (rejected in review)

| Don't do | Do instead |
|-----------|----------|
| `Command::new("codex")` or any spawn of a provider CLI | Call the transport directly |
| `if provider_id == "claude" { ... }` in `runtime` | Capability flag / `provider_options` / FeaturePack |
| Passing a `String` token through a function signature outside `auth` | `CredentialHandle` |
| `mpsc::unbounded_channel()` | `mpsc::channel(N)` + coalesce |
| `let out = child.wait_with_output()` then handing it all to the model | `OutputSpool` |
| Storing a provider's wire JSON in the DB | Store the canonical `protocol` type |
| Each agent creating its own `reqwest::Client` / spawning its own MCP server | Use the shared `Arc` from wiring |
| Loading the whole old session into `Vec<Message>` on resume | Lazy load via `ContextManager` + summaries |
| Returning a fake result when a feature isn't supported | `CommandResult::Unavailable` |
| Hard-coding an unverified endpoint/header | Record it in `docs/providers/*.md` after a spike, use `consts.rs` |
| Enabling an experimental transport in the default feature/config | Keep the D-002 gate |
