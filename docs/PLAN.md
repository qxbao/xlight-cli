# xlightcli — Technical Plan

> Version: **v0.3** (2026-09-28) — standardized all repository documentation to English (D-029); refined from the preliminary v0.1 draft (v0.2).
> Status: **Draft with direction locked in**, no code yet. Any architectural change must update this document + the decision log (§3).
>
> Related documents: [`../AGENTS.md`](../AGENTS.md) (rules for the coding agent),
> [`../CODEBASE.md`](../CODEBASE.md) (codebase map), [`../PATTERNS.md`](../PATTERNS.md) (code patterns).

---

## 1. Vision

`xlightcli` is a **standalone, lightweight, provider-aware coding-agent terminal, optimized for multi-agent use**.
It is not an "LLM proxy with a TUI bolted on" but rather:

```text
lightweight agent operating system for terminal
```

The terminal directly owns: authentication, upstream communication, session/conversation, tool execution,
MCP, agents/orchestration, provider-specific capabilities, and the TUI.

The provider's official CLI (`codex`, `claude`, `agy`) **is never part of the runtime path**.
They are only: a reference implementation, a source of config to import, a source of existing credentials, a compatibility target.

```text
┌──────────────────────────────────────────────────────────┐
│                           TUI                            │
│   prompt · agent tree · command palette · diff · logs    │
└────────────────────────────┬─────────────────────────────┘
                             │ RuntimeHandle / UiEvent
┌────────────────────────────▼─────────────────────────────┐
│                      Agent Runtime                       │
│ Session · AgentLoop · ContextManager · Scheduler         │
│ CommandRegistry · Permissions · Workspace · Hooks        │
├──────────────┬──────────────┬──────────────┬─────────────┤
│  ToolRegistry│  McpManager  │  AuthBroker  │  Storage    │
│  + Launcher  │  (shared)    │  (keyring)   │  (SQLite)   │
└──────────────┴──────┬───────┴──────┬───────┴─────────────┘
                      │ TurnRequest / AgentEvent (canonical)
        ┌─────────────┼──────────────┬──────────────┐
   ┌────▼─────┐  ┌────▼─────┐  ┌─────▼────┐
   │  codex   │  │  claude  │  │   agy    │   Provider = Auth + Transports + FeaturePack
   └────┬─────┘  └────┬─────┘  └────┬─────┘
   chatgpt (S)   anthropic-api (S)  antigravity (E)
   openai-api(S) claude-sub (E)     gemini-api (S)

   (S) = Stable transport   (E) = Experimental, feature-gated (see §15)
```

---

## 2. Invariants (hard requirements)

These are rules that **must not be violated**. Any PR that violates them must be rejected, regardless of the reason.

| ID | Invariant | Enforcement mechanism |
|----|-----------|--------------|
| INV-1 | Never spawn `codex` / `claude` / `agy` (or any provider CLI) as an execution backend. | `ProcessLauncher` is the single spawn point (clippy `disallowed-methods`) + blocklist + "shim binaries" integration test (§16). |
| INV-2 | The agent runtime is completely provider-independent: no `match provider_id` in the agent loop, no importing the `provider-*` crate from `runtime`/`tui`. | `cargo xtask check-deps` checks the dependency graph (CODEBASE.md §3). |
| INV-3 | Core only communicates through the canonical `TurnRequest` / `AgentEvent`; it never sees the provider's wire JSON. | Type boundary: wire types are `pub(crate)` within the adapter crate. |
| INV-4 | Raw credentials never leave the `auth` crate. No tokens in logs, plaintext SQLite, crash reports, argv, child-process env, MCP, or hooks. | `CredentialHandle` (no `Clone`/`Debug` that leaks the secret), `secrecy`, a redaction layer in tracing, env scrubbing in the launcher, tests. |
| INV-5 | One agent = one async task, not one OS process. | Review + benchmark process count. |
| INV-6 | Stream data uses a bounded channel / pull-based stream (backpressure). No `unbounded_channel()` for data. | clippy `disallowed-methods` for `tokio::sync::mpsc::unbounded_channel`. |
| INV-7 | Large tool output is never kept whole in the heap: stream → ring buffer (head+tail) → spilled to an artifact file. | `OutputSpool` is the only way a tool returns output. |
| INV-8 | MCP belongs to the core runtime; an MCP server is spawned only once (unless the isolation policy requires otherwise). | `McpManager` is the sole owner of the connection. |
| INV-9 | No quota bypass, no account rotation/pooling to dodge limits, no identity spoofing outside the scope of the experimental transport the user has opted into (§15). | Review + no API for multi-account-per-request. |
| INV-10 | An unsupported feature must clearly report "Unavailable", never fake the behavior. | `CommandResult::Unavailable` + test. |
| INV-11 | Core must survive a broken provider adapter: an adapter error must never panic/crash the runtime. | Adapter error → `ProviderError`; kill switch per transport. |

---

## 3. Decision log

Locked-in decisions. Add a new row when a decision changes (never delete the old row — mark it *superseded*).

| ID | Date | Decision | Rationale |
|----|------|-----------|-------|
| D-001 | 2026-09-28 | Project/binary name: **`xlightcli`**. Global config at `~/.config/xlightcli/`, project config at `<repo>/.xlightcli/`. Crate package prefix `xlightcli-`. | User decision. |
| D-002 | 2026-09-28 | **Experimental gate** for Claude/Antigravity subscriptions: disabled by default, compile-time feature + runtime opt-in with a ToS warning. Codex subscription (`chatgpt`) is the stable path. A valid API-key transport (`anthropic-api`, `gemini-api`, `openai-api`) is always available as a fallback. | Claude/Agy subscription transports may violate the ToS / require impersonating the official client; this conflicts with INV-9 if enabled by default. |
| D-003 | 2026-09-28 | Target for v0.x: **Linux + macOS**. Do not use APIs that would block a future Windows port (use abstractions for paths, process groups, keyring). | User decision. |
| D-004 | 2026-09-28 | Workspace of **~13 crates** following dependency boundaries (CODEBASE.md §2), submodules instead of sub-crates. | User settled on a "moderate" granularity; the 3 adapters are split into separate crates to isolate reuse/experimental code. |
| D-005 | 2026-09-28 | Rust edition 2024, Tokio multi-thread, ratatui + crossterm, reqwest (rustls + aws-lc-rs — reqwest 0.13 default provider; license ISC/Apache-2.0/MIT/BSD, GPLv3-compatible), serde, `rusqlite` (bundled, WAL) running on a dedicated persistence worker thread, the `keyring` crate, `tracing`, TOML. | Follows the v0.1 plan; `rusqlite` sync + worker thread is simpler than `sqlx` and fits the event-append workload. |
| D-006 | 2026-09-28 | Traits used via `dyn` (Provider, TransportAdapter, AuthAdapter, Tool, FeaturePack) use `#[async_trait]`. Traits used only generically may use native `async fn`. | Native async fn in trait is not yet dyn-compatible. |
| D-007 | 2026-09-28 | The stream from a transport is **pull-based**: `BoxStream<'static, Result<AgentEvent, ProviderError>>`. The adapter assembles deltas into a complete `Message` in the `Completed` event. | Natural backpressure; the runtime doesn't have to reassemble wire blocks. |
| D-008 | 2026-09-28 | No more `AgentEvent::ToolResultRequest` / `ContextUpdate`: a tool call ends the turn with `StopReason::ToolUse`, and the result is sent in the next `TurnRequest`. Context is the runtime's responsibility. | Removes ambiguous state from the v0.1 protocol. |
| D-009 | 2026-09-28 | Reasoning/thinking signature, encrypted reasoning, etc. are stored as an `OpaqueBlob` tagged with `(provider, transport)`. A blob may only be replayed to that exact transport. | Preserves continuity (Claude thinking signature, Codex encrypted reasoning, Gemini thought signature) without core needing to understand it. |
| D-010 | 2026-09-28 | No hot-switching providers mid-conversation. `/provider` creates a new session within the same workspace. | Opaque blobs + incompatible semantics. |
| D-011 | 2026-09-28 | Paths: use XDG on both Linux and macOS: config at `$XDG_CONFIG_HOME/xlightcli` (default `~/.config/xlightcli`), data at `$XDG_DATA_HOME/xlightcli` (`~/.local/share/xlightcli`), state/log at `$XDG_STATE_HOME/xlightcli`. | Predictable, matches common dev CLIs. |
| D-012 | 2026-09-28 | An agent's git worktree is placed **outside the repo**: `$DATA/worktrees/<repo-id>/<agent-name>`. | Avoids grep/IDE/indexer seeing duplicate files, and avoids needing to edit the user's `.gitignore`. |
| D-013 | 2026-09-28 | Grep/glob use libraries (`ignore`, `grep-searcher`, `globset`) — never spawn `rg`. Git uses the `git` CLI via `ProcessLauncher` in v0.x. | Fewer processes; the `git` CLI is the most correct choice for worktree/diff; may switch to `gix` after measuring. |
| D-014 | 2026-09-28 | MCP client: evaluate `rmcp` (the official Rust SDK) in Phase 3; wrap it behind an internal trait so it can be swapped out. | Don't hand-roll the protocol if the SDK is good enough. |
| D-015 | 2026-09-28 | *Docs-language part superseded by D-029.* Code, identifiers, comments, commit messages: **English**. Design documents in `docs/` and `*.md` files at the root: Vietnamese, keeping technical terminology as-is. Commits follow Conventional Commits. | Code is easier to reuse/read by tools and outsiders; docs serve the team. |
| D-016 | 2026-09-28 | Errors: `thiserror` in library crates, `anyhow` only in `app` and `xtask`. No `unwrap()`/`expect()` outside tests and genuine invariants (with a comment). | Adapter errors must be classifiable (INV-11). |
| D-017 | 2026-09-28 | Credential import is **import & own**: copy into xlightcli's secret store, refresh it ourselves, never write back to the provider CLI's files. Warn the user that refresh-token rotation may force the official CLI to log in again. | User decision (Q-3). Avoids a refresh-token race between the two clients; xlightcli does not depend on the CLI. |
| D-018 | 2026-09-28 | **OpenCodex** (<https://github.com/lidge-jun/opencodex>, MIT, TypeScript) is a reference + source for **selective porting** to Rust. Ports must keep the MIT notice (`THIRD_PARTY.md` + file header), record the origin commit, and live in the corresponding adapter crate. The list of modules that may/may not be ported: §4.5. | User decision (Q-1). MIT is compatible with GPL-2.0; being a different language, it should be a port, not a file copy. |
| D-019 | 2026-09-28 | Secret store **file** fallback (`$DATA/credentials/`, dir `0700`, file `0600`) for Linux without Secret Service. Enabled only via `auth.store = "file"` or interactive confirmation; always warns. | User decision (Q-2). |
| D-020 | 2026-09-28 | *Superseded by D-028.* Project license: **GPL-2.0** (SPDX `GPL-2.0-only`, header `// SPDX-License-Identifier: GPL-2.0-only` in every file). Dependencies must be GPL-2.0 compatible: allowlist in `deny.toml`; dual-licensed crates (`MIT OR Apache-2.0`) are used under the MIT branch; crates that are **only** Apache-2.0 (or `Apache-2.0 AND …`, OpenSSL license) are forbidden in the distributed binary. | User decision (Q-6). Apache-2.0 is not compatible with GPL-2.0-only — see R-9, Q-8. |
| D-021 | 2026-09-28 | `claude.insights` generates a self-contained **HTML file** (no external assets) at `$DATA/insights/<workspace-id>/insights-<timestamp>.html`; the command returns the path (the TUI displays an openable link). | User decision (Q-5), matching Claude Code's behavior. |
| D-022 | 2026-09-28 | Distribution: `cargo install` + GitHub Release binaries (Linux x86_64/aarch64, macOS aarch64), releases include a source tarball matching the tag (GPL obligation). | User decision (Q-7). |
| D-023 | 2026-09-28 | Command policy: **inherit as much as possible** of agy's / Claude Code's / Codex's commands so migration doesn't break workflows — keep the upstream names + aliases; commands common to all providers become `core.*`; provider-specific ones go into the FeaturePack. Full inventory at [`docs/commands.md`](commands.md). | User decision (Q-4). |
| D-024 | 2026-09-28 | A Compatible command is implemented as a provider-independent **Recipe** in `runtime::recipes` (prompt template + orchestration graph + artifact). The FeaturePack only declares the alias/default. `commands.cross_provider = true` allows calling another provider's recipe (e.g. `/agy:boost` on Claude). | Research shows agy's agent loop runs on the client (there's no `boost`/`goal`/`teamwork` field in the wire) ⇒ it can be reproduced and should not be locked to a vendor. |
| D-025 | 2026-09-28 | Add to Core the concepts all three CLIs share: **execution mode** (`default → accept-edits → plan`, Shift+Tab), **skills** (`SKILL.md` → slash command), **custom agents** (`agents/*.md`), **rules with frontmatter** (`always_on/glob/manual/model_decision`), **hooks** events `PreToolUse/PostToolUse/Stop/before_turn/after_turn`, **artifact review** (plan/diff artifacts with approve/comment), **checkpoint** for `/rewind`. | Follows from the inherit-workflow decision (D-023); without them, migration would break. |
| D-026 | 2026-09-28 | `xlightcli exec` is compatible with agy's/Claude Code's print-mode flags (`-p`, `--output-format text\|json\|stream-json`, `-c`, `--model`, `--mode`, `--agent`, exit codes 0/1/2/3) — details in `docs/commands.md` §5. | Users' scripts/CI can migrate without modification. |
| D-027 | 2026-09-28 | `TransportAdapter` gains a `quota()` method; each `Provider` may supply a `ConfigImporter` (the vendor config format lives in the adapter crate, returning a canonical fragment). | `/usage` is Native on all 3 backends; `/import` doesn't make core vendor-specific. |
| D-028 | 2026-09-28 | Project license: **GPL-3.0-only** (SPDX header `// SPDX-License-Identifier: GPL-3.0-only`, `[workspace.package] license = "GPL-3.0-only"`). Apache-2.0 dependencies are allowed (compatible with GPLv3). Porting code from Apache-2.0 projects (e.g. `openai/codex`) is allowed if the license notice + NOTICE are kept and changes are recorded in `THIRD_PARTY.md`. | User decision (replaces D-020, closes Q-8). |
| D-029 | 2026-09-28 | All repository documentation (root *.md, docs/**) and code comments are written in **English**. Replies to the user may still be in Vietnamese. | User decision; standardizes the repo for external contributors and tools. |

---

## 4. Provider layer

### 4.1 Model: Provider = Auth + Transports + FeaturePack

The three concerns of **authentication**, **wire protocol**, and **CLI feature** are independent. A provider can have
multiple transports (subscription vs API key) sharing a wire translator.

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;                       // "codex" | "claude" | "agy"
    fn display_name(&self) -> &str;
    fn auth(&self) -> &dyn AuthAdapter;
    fn transports(&self) -> &[Arc<dyn TransportAdapter>];
    fn features(&self) -> &dyn ProviderFeaturePack;
    fn importer(&self) -> Option<&dyn ConfigImporter> { None }   // D-027, docs/import.md

    fn transport(&self, id: &TransportId) -> Option<&Arc<dyn TransportAdapter>> {
        self.transports().iter().find(|t| &t.id() == id)
    }
}

#[async_trait]
pub trait TransportAdapter: Send + Sync {
    fn id(&self) -> TransportId;                      // "chatgpt", "anthropic-api", ...
    fn stability(&self) -> Stability;                 // Stable | Experimental
    fn required_auth(&self) -> AuthKind;              // Subscription | ApiKey
    fn capabilities(&self) -> &ProviderCapabilities;
    fn protocol_version(&self) -> ProtocolVersion;    // version gating

    async fn list_models(&self, cred: &CredentialHandle) -> Result<Vec<ModelInfo>, ProviderError>;

    /// Plan/quota snapshot for `/usage` (D-027). `Ok(None)` when upstream exposes nothing.
    async fn quota(&self, cred: &CredentialHandle) -> Result<Option<QuotaSnapshot>, ProviderError>;

    /// Maps canonical `ReasoningEffort` to wire fields / model id variants (agy encodes tier in model id).
    fn apply_effort(&self, req: &mut TurnRequest, effort: ReasoningEffort);

    async fn stream(
        &self,
        req: TurnRequest,
        cred: CredentialHandle,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError>;
}

pub type EventStream = BoxStream<'static, Result<AgentEvent, ProviderError>>;
```

### 4.2 Transport matrix

| Provider | Transport | Auth | Stability | Wire (hypothesis — verify in Phase 0) |
|----------|-----------|------|-----------|------------------------------------------|
| `codex` | `chatgpt` | ChatGPT OAuth (PKCE, loopback) | Stable | OpenAI Responses-style SSE via the ChatGPT/Codex backend |
| `codex` | `openai-api` | API key | Stable (optional) | OpenAI Responses API |
| `claude` | `anthropic-api` | API key | Stable | Anthropic Messages API SSE |
| `claude` | `claude-subscription` | Claude OAuth | **Experimental** | Anthropic Messages API + OAuth bearer |
| `agy` | `antigravity` | Google OAuth | **Experimental** | Cloud Code Assist (wrap Gemini `generateContent`) |
| `agy` | `gemini-api` | API key | Stable | Gemini API `streamGenerateContent` |

The wire translator is shared within the same adapter crate: `claude-subscription` and `anthropic-api` share the
Messages translator; `antigravity` and `gemini-api` share the Gemini translator.

The official CLI's endpoint, headers, OAuth client, and credential format **may only be written into
`docs/providers/<provider>.md` after the Phase 0 spike verifies them**, along with the verification date and protocol version.

### 4.3 Canonical protocol (crate `protocol`)

```rust
pub struct TurnRequest {
    pub model: ModelId,
    pub system: SystemPrompt,             // includes resolved project rules
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub reasoning: Option<ReasoningConfig>,
    pub max_output_tokens: Option<u32>,
    pub provider_options: ProviderOptions, // typed per transport, opaque to core
}

pub struct Message {
    pub role: Role,                       // System | User | Assistant
    pub content: Vec<ContentBlock>,
}

pub enum ContentBlock {
    Text { text: String },
    Image { media_type: String, source: ImageSource },          // ArtifactRef or small inline bytes
    Reasoning { text: Option<String>, opaque: Option<OpaqueBlob> },
    ToolUse { id: ToolCallId, name: String, input: serde_json::Value },
    ToolResult { call_id: ToolCallId, content: Vec<ToolResultPart>, is_error: bool },
}

/// Provider-specific continuity data (thinking signature, encrypted reasoning, ...).
/// Only replayed to the same (provider, transport).
pub struct OpaqueBlob {
    pub provider: ProviderId,
    pub transport: TransportId,
    pub data: serde_json::Value,
}

pub enum AgentEvent {
    TurnStarted { model: ModelId },
    TextDelta { index: u32, text: String },
    ReasoningDelta { index: u32, text: String },
    ToolCallStarted { index: u32, id: ToolCallId, name: String }, // UI only
    Usage(Usage),                          // may appear multiple times (incremental)
    RateLimit(RateLimitInfo),              // from header/metadata if upstream provides it
    Completed { message: Message, stop: StopReason, usage: Usage },
}

pub enum StopReason { EndTurn, ToolUse, MaxTokens, Refusal, Cancelled, Other(String) }
```

Errors flow through the stream's `Result::Err(ProviderError)`, not through an event variant:

```rust
pub enum ProviderError {
    Auth(AuthFailure),                 // 401/403 → broker refreshes once then reports to the user
    RateLimited { retry_after: Option<Duration>, info: RateLimitInfo },
    QuotaExhausted { resets_at: Option<SystemTime> },
    InvalidRequest(String),
    ProtocolMismatch { expected: ProtocolVersion, detail: String }, // version gating
    TransportDisabled { reason: String },                            // kill switch
    Network(String),
    Upstream { status: u16, body_excerpt: String },                  // body already redacted
    Cancelled,
}
```

### 4.4 Capability manifest

```rust
pub struct ProviderCapabilities {
    pub reasoning: bool,
    pub images: bool,
    pub tool_calls: bool,
    pub parallel_tool_calls: bool,
    pub web_search: CapabilityMode,
    pub mcp: CapabilityMode,           // always Core in v0.x
    pub session_resume: CapabilityMode,
    pub usage: CapabilityMode,
    pub quota: CapabilityMode,
    pub context_window: Option<u32>,
}

pub enum CapabilityMode { Native, Core, Compatible, Unsupported }
```

`/provider info` renders the manifest of the **currently active transport** + the FeaturePack's command list with the corresponding mode.

### 4.5 OpenCodex port map (D-018)

OpenCodex is a TypeScript **proxy** (Codex/Claude Code → another provider), so only the auth/wire parts are of value to
xlightcli. The map below is based on the directory structure as of 2026-09-28; the file contents must be read to confirm
during the Phase 0 spike before porting. Each port must record the original commit SHA in `THIRD_PARTY.md`.

| OpenCodex source (`src/…`) | xlightcli destination | Notes |
|---------------------------|----------------|---------|
| `oauth/pkce.ts`, `oauth/callback-server.ts`, `oauth/open-browser-choice.ts` | `auth::oauth` | PKCE, loopback callback, opening the browser |
| `oauth/token-guardian.ts`, `oauth/store.ts` | `auth::refresh`, `auth::store` (idea) | Cross-check against our single-flight implementation |
| `oauth/local-token-detect.ts`, `oauth/account-import/*` | `auth::discovery` + `provider-*::auth` | Discovery/import of the official CLI's credentials |
| `oauth/chatgpt.ts`, `oauth/chatgpt-device.ts`, `codex/refresh.ts` | `provider-codex::auth` | ChatGPT OAuth, device code, refresh |
| `adapters/openai-responses*.ts`, `adapters/openai-responses/*` (reasoning, tool-schema, prompt-cache, web-search) | `provider-codex::wire` | Responses wire ↔ canonical |
| `oauth/anthropic.ts` | `provider-claude::auth` (feature `claude-subscription`) | Experimental |
| `adapters/anthropic.ts`, `adapters/anthropic/*`, `adapters/anthropic-image-*.ts`, `adapters/anthropic-output-schema.ts` | `provider-claude::wire` | Messages wire, image, beta headers |
| `claude/context-windows.ts`, `claude/model-info.ts` | `provider-claude` model metadata | |
| `oauth/google-antigravity.ts`, `oauth/account-import/google-antigravity-adapter.ts` | `provider-agy::auth` (feature `antigravity-subscription`) | Experimental |
| `adapters/google*.ts` (`google-antigravity-wire`, `-replay`, `wire-compiler`, `wire-shape`, `tool-schema`, `truncation`, `errors`, `http`) | `provider-agy::wire` | Gemini / Cloud Code Assist wire |
| `providers/antigravity-models.ts`, `providers/quota/antigravity.ts` | `provider-agy` models + quota (`/usage`) | Native if the backend exposes it |
| `bridge/sse.ts`, `adapters/upstream-http-error.ts` | `provider::sse`, `provider::error` | SSE parser + upstream error mapping |

**Not ported** (violates an invariant or unnecessary for a standalone terminal):

| Source | Reason |
|-------|-------|
| `codex/account-*`, `oauth/pool-kernel.ts`, `oauth/account-quota-rank.ts`, `oauth/generic-account-failover.ts`, `*-failover.ts`, `quota/*` auto-switch parts | Account pooling / rotation to dodge limits — INV-9 |
| `adapters/claude-cli/*` | Calls the Claude CLI — INV-1 |
| `claude/intercept/*` (local CA, CONNECT proxy), `claude/desktop-*` | Client injection / intercept |
| `server/`, `app/`, `desktop/`, `tray/`, `cli/codex-shim-*`, `remote-control/`, `update/` | Proxy lifecycle, dashboard, GUI |
| `combos/`, `routing/` | Failover/round-robin between providers; only for reference when building model routing (Phase 6) |

---

## 5. Feature classification & command system

### 5.1 Four feature types

| Mode | Definition | Example |
|------|-----------|-------|
| **Native** | Upstream genuinely exposes the capability; we call the protocol directly. | Tool calls, reasoning, usage/rate-limit header. |
| **Core** | The runtime implements it itself, provider-independent. | `/mcp`, `/agents`, `/context`, `/model`, `/provider`, `/tools`, `/permissions`, `/compact`, `/resume`. |
| **Compatible** | Exists in the provider's CLI but is mostly client-side; we reimplement compatible behavior. | Claude `/insights`. |
| **Unsupported** | Depends on an inaccessible backend / proprietary / unclear contract / not allowed by policy. | Displays `Unavailable with current provider adapter.` |

Criteria for porting a feature (all must hold): it has value; the semantics are well enough understood; it doesn't require
running the official CLI; it doesn't make core vendor-specific; it is testable.

### 5.2 FeaturePack

```rust
#[async_trait]
pub trait ProviderFeaturePack: Send + Sync {
    fn commands(&self) -> Vec<CommandDefinition>;   // id namespaced, alias, mode, required transport
    async fn execute(&self, cmd: ProviderCommand, ctx: CommandContext) -> Result<CommandResult, CommandError>;
}

pub struct CommandDefinition {
    pub id: CommandId,                     // "claude.insights"
    pub alias: &'static str,               // "insights"
    pub mode: CapabilityMode,
    pub requires_transport: Option<TransportId>, // None = every transport of the provider
    pub summary: &'static str,
}

pub enum CommandResult {
    Message(RichText),
    StartTurn(TurnRequestPatch),           // command turns into a prompt (e.g. /review)
    Unavailable { reason: String },
    // ...
}
```

`CommandContext` only exposes safe APIs: reading session history (via storage query), workspace info, sending UI output,
running an auxiliary model turn via the runtime. **No** credentials.

### 5.3 Namespace & resolution

- Internal IDs are always namespaced: `core.mcp`, `core.model`, `claude.insights`, `agy.boost`, `codex.review`.
- The short alias `/insights` resolves in order: **core → the active provider's FeaturePack**. Core wins on conflict
  (the provider command can still be invoked via `/claude:insights`).
- The explicit syntax `/<provider>:<alias>` always works; if that provider isn't active → `Unavailable`
  with a suggestion to run `/provider`.
- Autocomplete only shows commands applicable to the currently active transport.

### 5.4 Command inventory

Full list (core + agy/Claude/Codex FeaturePack, mode, confidence level, phase): [`commands.md`](commands.md).
Config import mapping: [`import.md`](import.md).

Priority order:

1. Phase 1: minimal core session/UI commands (`/help`, `/exit`, `/clear`, `/resume`, `/model`, `/context`, `/compact`, `/diff`, `/permissions`, `/config`, `/status`, `/login`, `/logout`, Shift+Tab mode).
2. Phase 2: shared lightweight recipes (`/plan`, `/goal`, `/btw`, `/grill-me`, `/review`, `/init`, `/learn`, `/recap`), `/usage` Native, `/effort`, `/fork`, `/rewind`, `claude.insights`.
3. Phase 3: `/mcp`, `/import`.
4. Phase 4–5: multi-agent recipes (`agy.boost`, `agy.teamwork`), `/agents`, `/tasks`.
5. Phase 6: skills/hooks/plugins/statusline/keybindings/schedule/browser.

---

## 6. Authentication

### 6.1 Components

```text
AuthBroker (crate auth)
├── AccountIndex        metadata in SQLite: provider, account_id, auth_kind, expiry, keyring_ref
├── SecretStore         OS keyring (Secret Service / macOS Keychain); file fallback 0600 when opted in (D-019)
├── RefreshCoordinator  single-flight refresh per account + proactive refresh before expiry
├── OAuth toolkit       PKCE + loopback redirect, device code, browser open
└── Discovery           reads the official CLI's credentials/config (read-only)
```

```rust
#[async_trait]
pub trait AuthAdapter: Send + Sync {
    fn methods(&self) -> &[AuthMethod];               // ReuseExisting | BrowserOAuth | DeviceCode | ApiKey
    async fn discover_existing(&self) -> Vec<DiscoveredCredential>;
    async fn import(&self, found: &DiscoveredCredential) -> Result<CredentialSet, AuthError>;
    async fn login(&self, method: AuthMethod, ui: &dyn LoginUi) -> Result<CredentialSet, AuthError>;
    async fn refresh(&self, current: &CredentialSet) -> Result<CredentialSet, AuthError>;
    async fn revoke(&self, current: &CredentialSet) -> Result<(), AuthError>;
}
```

`AuthAdapter` is supplied by the provider (it knows the specific flow); **`AuthBroker` is the sole owner** of storage, refresh,
and locking. A transport never sees `CredentialSet`, only receives a `CredentialHandle`.

### 6.2 CredentialHandle

```rust
/// Opaque, cheap-to-clone reference to a live credential owned by AuthBroker.
/// Never exposes the raw secret outside crate `auth`.
pub struct CredentialHandle { /* Arc<AccountEntry> */ }

impl CredentialHandle {
    pub fn account(&self) -> &AccountInfo;                         // non-secret metadata
    pub async fn authorize(&self, headers: &mut HeaderMap) -> Result<(), AuthError>; // inserts the bearer/API key
    pub async fn on_unauthorized(&self) -> Result<(), AuthError>;  // single-flight refresh, called on 401
}
```

The handle's `Debug` only prints `provider/account_id`. The internal secret is wrapped in `secrecy::SecretString` + zeroize.

### 6.3 Priority flow

```text
discover_existing() ── found ──▶ user selects "Reuse existing login" ──▶ import & own (D-017)
        │
        └─ not found / user chooses otherwise ──▶ log in directly (browser PKCE loopback → device code fallback)
                                   or API key (stable transport of claude/agy)
```

- Never runs `codex login` / `claude login` / `agy login` in any form.
- The terminal must work fully when all 3 binaries are absent (mandatory integration test).
- Refresh: proactive (~5 minutes before expiry) + reactive (401 → refresh once → retry once → report an error).
  20 agents sharing an account ⇒ **one** refresh request (single-flight).

---

## 7. Tools, permissions, process launching

### 7.1 Tool trait

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> &ToolDefinition;
    fn effect(&self, input: &Value) -> ToolEffect;     // ReadOnly | WritesWorkspace | Executes | Network
    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError>;
}
```

`ToolContext` consists of: workspace root (shared checkout or worktree), `PermissionGate`, `OutputSpool`,
`ProcessLauncher`, `CancellationToken`. **No** credentials.

### 7.2 Built-in tools (Phase 1)

`read_file`, `write_file`, `edit_file` (exact string replace), `list_dir`, `glob`, `grep` (the `ignore` +
`grep-searcher` libraries), `shell`, `git_diff`/`git_status`. Phase 4 adds orchestration tools: `spawn_agent`,
`send_agent`, `wait_agents`, `cancel_agent`.

Read-only tool calls within the same assistant message run in parallel; tools with side effects run sequentially in the order the model returned them.

### 7.3 Permissions

- Execution mode (Shift+Tab, D-025): `default` · `accept-edits` · `plan` (read-only tools only, ends with a plan artifact).
- Permission preset: `read-only` · `strict` · `ask` (default) · `auto-edit` · `full-auto`; `sandboxed-auto` once an OS sandbox exists. Mapping to agy/CC/Codex: `docs/commands.md` §4.
- Rule format `action(target)` with `*` and `regex:`; priority is deny > ask > allow.
- Rule: `allow|deny|ask` per `tool` + pattern (path glob, command prefix). Deny beats allow.
- Project config **must not** raise the permission mode or add its own allow rule for exec before it is trusted (§12.3).
- OS-level sandbox (Landlock/bubblewrap on Linux, Seatbelt on macOS) deferred to after Phase 6; `ProcessLauncher` is designed so a sandbox wrapper can be inserted.

### 7.4 ProcessLauncher

The **single** process-spawn point for the entire codebase (shell tool, MCP stdio, hooks, git).

- `SpawnPurpose::{ShellTool, Mcp, Hook, Git}`.
- For `Mcp | Hook | Git`: rejects any program basename on the provider-CLI blocklist (`codex`, `claude`, `agy`, `antigravity`) — defense in depth for INV-1.
- Env scrub: strips variables containing credentials that xlightcli uses (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `XLIGHTCLI_*_TOKEN`, …); passthrough only when the user explicitly declares it in config.
- A dedicated process group, timeout, kills the whole group on cancel.
- stdout/stderr → `OutputSpool`.

### 7.5 OutputSpool

- Keeps a head (e.g. 8 KiB) + tail ring buffer (e.g. 32 KiB) in RAM; the full output is written straight to
  `$DATA/artifacts/<session>/<call-id>.log`.
- The model receives: head + tail + total byte/line count + `ArtifactRef`. The model can read further via `read_file` with a range on the artifact.
- A 200 MB output ⇒ RAM grows no more than the buffer size.

---

## 8. MCP

```text
Agent A ─┐
Agent B ─┼── McpManager ── ConnectionPool ── MCP server (spawned once)
Agent C ─┘
McpManager
├── ConfigRegistry     normalizes from global/project/import (Claude/Codex/Agy)
├── ConnectionPool     lazy connect, health check, restart with backoff
├── ToolBridge         MCP tool → Tool trait, namespaced name `mcp__<server>__<tool>`
├── PermissionLayer    MCP tools default to `ask`; effect declared in config
└── CredentialBroker   SEPARATE OAuth/token for the MCP server (isolated from provider credentials)
```

Config normalization:

```toml
[mcp.github]
transport = "stdio"
command = "github-mcp-server"
args = []
env_passthrough = ["GITHUB_TOKEN"]

[mcp.docs]
transport = "http"            # streamable HTTP
url = "https://example.com/mcp"
```

- An MCP server from project config is only started after the workspace is trusted.
- Provider credentials are **never** placed into the env/headers of an MCP server (INV-4).
- `/mcp` behaves identically regardless of provider (Core).

---

## 9. Agent runtime

### 9.1 Agent loop (provider-independent)

```text
loop {
    req    = context_manager.build(session, agent_profile)   // rules + history + tools, compaction if needed
    permit = scheduler.llm_permit(transport, account).await
    stream = transport.stream(req, credential_handle, cancel)
    for event in stream:
        persist (batched) + forward UiEvent
    match stop {
        ToolUse   => results = tool_executor.run(calls, permissions).await; append ToolResult; continue
        EndTurn   => break
        MaxTokens => continue-or-stop according to policy
        Cancelled => break
    }
    budget.check()?   // token / turn / wall-clock
}
```

There is no `if provider == ...` branch in the loop. Provider differences flow only through `ProviderCapabilities`
(e.g. `parallel_tool_calls`, `images`) and `provider_options`.

### 9.2 ContextManager

- Resolving project rules reads directly from the standard paths shared across CLIs (`AGENTS.md`/`AGENTS.override.md` from the repo root down to cwd, `CLAUDE.md`, `GEMINI.md`, `.agents/rules/*.md`, `.xlightcli/rules/*.md`) per `rules.sources`; frontmatter `trigger: always_on|glob|manual|model_decision` (D-025, `docs/import.md`).
- Skills (`.agents/skills`, `.claude/skills`, …) load only metadata (name/description) into context; content is loaded when invoked.
- Token estimation (tokenizer heuristic + actual usage from the provider for calibration).
- Compaction when a threshold is exceeded (default 80% of the context window): summarize old turns via an auxiliary model turn, store `summaries`, keep the most recent turns and valid opaque blobs intact.
- Old tool output is replaced with a stub + `ArtifactRef` before it needs to be summarized.
- `/context` shows a breakdown (system, rules, history, tools, tool outputs).

### 9.3 Headless mode

`xlightcli exec "<prompt>" [--provider X] [--json]` runs the same runtime, no TUI, streams events to stdout
(JSONL when `--json`). Used for CI, benchmarking, and e2e tests.

---

## 10. Multi-agent

### 10.1 Model

```rust
pub struct Agent {
    pub id: AgentId,
    pub parent: Option<AgentId>,
    pub profile: AgentProfile,        // provider/transport/model/tools/permission
    pub workspace: WorkspaceId,
    pub session: SessionId,
    pub state: AgentState,            // Queued | Running | WaitingTool | WaitingPermission | Done | Failed | Cancelled
    pub budget: Budget,
}
```

Agent = actor (tokio task) with a bounded mailbox. Cancellation follows the tree: a `CancellationToken` child of the parent.

### 10.2 Primitives

`spawn`, `send`, `wait` (one/many/any), `join`, `cancel`, `delegate` (= spawn with a profile + wait for the result).
Exposed to the model via orchestration tools; exposed to the user via `/agents` and the agent tree UI.

A child returns a compact result — the parent **does not** inherit the child's transcript:

```rust
pub struct AgentResult {
    pub summary: String,
    pub findings: Vec<Finding>,
    pub files_changed: Vec<PathBuf>,
    pub artifacts: Vec<ArtifactRef>,
    pub usage: Usage,
    pub outcome: AgentOutcome,        // Completed | Failed(reason) | Cancelled | BudgetExceeded
}
```

### 10.3 Scheduler

```toml
[agents]
max_active = 8
max_depth = 4

[concurrency]
llm_requests = 4          # per (transport, account)
shell_jobs = 4
browser_jobs = 1

[budget.default]
max_turns = 50
max_output_tokens = 200_000
max_wall_clock = "30m"
```

A dedicated semaphore per resource type. Rate limit (429) → backoff per `retry_after`, **never** switching to another account (INV-9).

---

## 11. Memory, persistence, workspace

### 11.1 Memory strategy

Hot state (RAM): current turn, recent context window, in-flight tool calls, stream buffer, agent metadata.
Cold state (SQLite + artifact files): everything else. Old sessions are **not** loaded entirely back into RAM; lazily paged from storage.

Process-wide shared resources: `reqwest::Client` (connection/TLS pool) per transport, MCP connections,
repository index, git metadata cache, file watcher. Never cloned per agent.

### 11.2 Persistence

SQLite WAL, a single writer thread (persistence worker) receives commands via a bounded channel, batch inserts
(flushed by count or every 50 ms). Readers use a separate connection.

Preliminary schema (versioned migrations):

```text
workspaces(id, root, repo_id, created_at)
sessions(id, workspace_id, provider, transport, model, title, created_at, updated_at, status)
agents(id, session_id, parent_id, profile_json, state, created_at, finished_at)
events(id, session_id, agent_id, seq, kind, payload_json, created_at)     -- append-only, source of truth
messages(id, session_id, agent_id, turn, role, content_json)              -- projection from events
tool_calls(id, agent_id, name, input_json, status, artifact_id, started_at, finished_at)
artifacts(id, session_id, path, bytes, sha256, kind)
summaries(id, session_id, agent_id, covers_until_seq, text)
provider_usage(id, session_id, agent_id, transport, model, input_tokens, output_tokens, cached_tokens, at)
accounts(id, provider, auth_kind, account_label, expiry, keyring_ref, metadata_json)  -- NO secrets
```

Event-oriented: `TurnStarted → TextDelta* (batched by chunk) → ToolCalled → ToolCompleted → TurnCompleted → AgentCompleted`.
After a crash: reconstruct `messages` from `events`; an in-progress turn is marked `Interrupted`.

### 11.3 Workspace isolation

| Agent type | Default workspace |
|-----------|--------------------|
| read-only | shared checkout |
| writing | a dedicated git worktree at `$DATA/worktrees/<repo-id>/<agent>` (D-012), branch `xlightcli/<session>/<agent>` |
| review | configurable (default shared, read-only) |

Lifecycle (Phase 5): create → agent works → diff/review in the TUI → merge (fast-forward/cherry-pick/patch) or discard → cleanup. Never auto-merges without user confirmation.

---

## 12. Configuration & customization

### 12.1 Paths (D-011)

```text
~/.config/xlightcli/config.toml          global config
~/.local/share/xlightcli/                xlightcli.db, artifacts/, worktrees/
~/.local/state/xlightcli/logs/           logs (redacted)
<repo>/.xlightcli/config.toml            project config (committable)
<repo>/.xlightcli/config.local.toml      personal override (should be gitignored)
<repo>/.xlightcli/rules/*.md             additional project rules
```

### 12.2 Layering (deterministic)

```text
built-in defaults
  → ~/.config/xlightcli/config.toml
  → <repo>/.xlightcli/config.toml
  → <repo>/.xlightcli/config.local.toml
  → env XLIGHTCLI_*
  → CLI flags / session override
```

Merge rule: table deep-merge; scalars overwrite; arrays are **replaced** (not concatenated). `xlightcli config show --origin`
prints values together with their source.

### 12.3 Workspace trust

The first time a repo with `.xlightcli/` is opened: ask for trust. While not trusted, the following are ignored from
project config: hooks, MCP stdio servers, allow rules for exec, permission mode > `ask`, enabling experimental transports, env passthrough.

### 12.4 Main config blocks

```toml
default_provider = "codex"

[provider.codex]
transport = "chatgpt"
default_model = "..."

[provider.claude]
transport = "anthropic-api"          # "claude-subscription" requires [experimental]
default_model = "..."

[provider.agy]
transport = "gemini-api"             # "antigravity" requires [experimental]
default_model = "..."

[experimental]
claude_subscription = false          # only takes effect from global config + after being acknowledged
antigravity_subscription = false

[agent.reviewer]
provider = "claude"
model = "..."
tools = ["read_file", "grep", "git_diff"]
permission = "read-only"

[keybind]
new_agent = "ctrl+n"
agent_tree = "ctrl+a"
command_palette = "ctrl+p"

[hooks]
before_tool = "..."                  # v0.x: shell command, receives JSON via stdin, scrubbed env
after_turn = "..."
agent_complete = "..."
```

A sub-agent may override the provider only when `agents.allow_provider_override = true`.

---

## 13. Extensibility

- v0.x: built-in provider traits + config + hooks. **No** exposed Rust dylib ABI.
- Once the interface stabilizes (after Phase 6): WASM plugins (command/provider) with a capability sandbox
  (`network`, `filesystem`, `secrets`, `tools`). Plugins cannot read subscription credentials by default.

---

## 14. Security rules

Mandatory (tied to INV-4):

- No tokens in logs / plaintext SQLite / crash reports / argv / child-process env / MCP / hooks.
- `tracing` has a redaction layer (patterns `Bearer …`, `sk-…`, JWT, refresh token field) — defense in depth, not a substitute for never logging secrets.
- HTTP error bodies only log a redacted excerpt.
- The OAuth loopback server binds `127.0.0.1`, a random port (or the client's required port), validates `state`, uses PKCE S256, and closes immediately after the callback.
- File fallback secret store (if opted in): `0600` permissions, `0700` directory, clear warning.
- The panic hook never prints env/config containing secrets.
- `cargo deny` for license + advisory checks. License allowlist per D-028 (GPLv3 compatible): `MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `ISC`, `BSD-2-Clause`, `BSD-3-Clause`, `Zlib`, `Unicode-3.0`, `Unicode-DFS-2016`, `Unlicense`, `CC0-1.0`, `MPL-2.0`, `GPL-3.0-*`, `LGPL-*`, `BSL-1.0`, `CDLA-Permissive-2.0`; an `OR` expression is allowed if at least one branch is in the allowlist. Forbidden: the old OpenSSL license (SSLeay), non-free licenses.

---

## 15. Provider policy risk & experimental gate (D-002)

Subscription support is a **compatibility surface**, not a stable public API. Core must survive a broken adapter.

Every subscription transport needs:

| Mechanism | Description |
|--------|-------|
| Compile-time feature | `claude-subscription`, `antigravity-subscription` (disabled in the release's default features until decided otherwise). |
| Runtime opt-in | `[experimental]` in the **global** config + an acknowledgement stored with the warning version (shows a ToS dialog the first time). |
| Version gating | Adapter pins a `ProtocolVersion`; an unfamiliar response schema ⇒ `ProtocolMismatch`, never guessed. |
| Protocol tests | Redacted SSE fixtures for each transport (§16). |
| Kill switch | `xlightcli provider disable <transport>` + config `disabled_transports = [...]`; repeated adapter errors ⇒ auto-disable for the session and notify the user. |
| Clear errors | Every error displays the transport, a classified cause, and a fallback suggestion (e.g. switch to `anthropic-api`). |

Not implemented: quota bypass, account rotation/pooling, fake quota info, sending requests on behalf of another user.

Codex `chatgpt` is stable but must still have all the mechanisms above (except the compile-time gate).

---

## 16. Testing strategy

| Layer | Tooling | Content |
|------|---------|----------|
| Unit | `cargo test` / `cargo nextest` | Every crate. |
| Wire translation | redacted SSE/JSON fixtures in `crates/provider-*/tests/fixtures/`, `insta` snapshots | wire → `AgentEvent` sequence; `TurnRequest` → wire body. |
| Transport | `wiremock` | 401 → refresh → retry; 429 + `retry_after`; stream cut off mid-way; cancel; unfamiliar schema ⇒ `ProtocolMismatch`. |
| Auth | mock OAuth server | PKCE, loopback, device code, single-flight refresh (N concurrent tasks ⇒ 1 refresh). |
| Runtime | `MockProvider` (scripted events) | Agent loop, tool execution, compaction, budget, cancel, multi-agent. No network. |
| E2E | `xlightcli exec` + mock provider HTTP server | Full headless flow. |
| **No-provider-CLI** | PATH contains shims for `codex`/`claude`/`agy` that write a marker file when invoked | Run e2e with 20 agents + MCP; assert the marker doesn't exist and the process tree doesn't contain those names. **Mandatory in CI.** |
| Credential leak | test with a secret sentinel | The sentinel doesn't appear in logs, DB, artifacts, or child env. |
| Live | `#[ignore]` + `XLIGHTCLI_LIVE_<PROVIDER>=1` | Run manually with a real account; never run in CI. |

Fixtures never contain real tokens/account ids; the `xtask redact-fixture` script is mandatory before committing.

---

## 17. Performance benchmarking

Measured starting in Phase 1, reproducible via `cargo xtask bench-mem` with a mock provider server (SSE replay, fixed rate).

Scenarios: startup; 1 / 10 idle sessions; 1 / 5 / 20 streaming agents; MCP enabled (3 servers); large repo (grep/glob);
large tool output (200 MB).

Metrics: RSS, peak RSS (`VmHWM`), startup latency, time-to-first-token overhead (versus a direct request to the mock),
idle CPU, number of processes, number of open connections.

Compared against the same workload on Codex CLI, Claude Code, Agy (run manually, results recorded in `docs/benchmarks/`).

Starting targets (to be calibrated after the first measurement in Phase 1):

| Metric | Target |
|--------|----------|
| Startup RSS (TUI idle, no MCP) | ≤ 30 MB |
| Each additional idle agent | ≤ 1 MB |
| Each additional streaming agent (excluding context text) | ≤ 3 MB |
| 200 MB tool output | peak RSS increase ≤ 5 MB |
| TTFT overhead | ≤ 20 ms |
| Idle CPU | ~0% (no busy-loop, TUI redraws only on event/tick) |

Principle: `memory ≈ base runtime + active workload`, not `N agents × full CLI runtime`.

---

## 18. UX

### 18.1 Init

```text
$ xlightcli init

Select provider
  1. Antigravity (agy)
  2. Claude
> 3. Codex

Authentication
> Reuse existing login          (found: ~/.codex/…)      ← only shown when discovery finds one
  Login with browser
  Use API key                                              ← stable transport for claude/agy

Account · Provider · Transport · Plan/quota (if available) · Available models

Import configuration?
  [x] MCP servers
  [x] project rules (AGENTS.md / CLAUDE.md / GEMINI.md)
  [ ] provider-specific settings

→ writes .xlightcli/config.toml (project) and/or ~/.config/xlightcli/config.toml
```

Choosing an experimental transport ⇒ shows a ToS warning + requires confirmation, recorded in the global config.

### 18.2 Runtime

```text
~/repo · Codex/chatgpt · gpt-… · 3 agents (2 running) · ctx 41% · ask

/root                ● working
├─ backend           ● working
├─ tests             ✓ done
└─ review            ◌ queued
```

Autocomplete only shows applicable commands (`> /in` → `/insights` on Claude; `> /bo` → `/boost` on agy).

### 18.3 CLI surface

```text
xlightcli                       TUI
xlightcli init                  wizard
xlightcli exec "<prompt>"       headless
xlightcli auth {list,login,logout,import}
xlightcli provider {list,info,disable,enable}
xlightcli config {show,path}
xlightcli dev probe <provider> [--transport T] "<prompt>"   (Phase 0 deliverable)
```

---

## 19. Development phases

Every phase has **exit criteria**; the next phase does not start until the previous one is met.

### Phase 0 — Protocol/Auth spike

Sole goal: `login → refresh → send prompt → stream response` for all 3 providers, no TUI/agent/MCP.

Work to do:
1. Scaffold the minimal workspace: `protocol`, `auth`, `provider`, `provider-{codex,claude,agy}`, `app` (only `dev probe`), `xtask`.
2. For each provider: research it (the official CLI as reference, OpenCodex as reference/port source per §4.5), write `docs/providers/<provider>.md` (auth flow, endpoint, headers, stream format, credential location, protocol version, verification date).
3. Implement stable transports first: `chatgpt`, `anthropic-api`, `gemini-api`; then experimental `claude-subscription`, `antigravity`.
4. Common smoke test:

```rust
async fn provider_smoke_test(
    broker: &AuthBroker,
    provider: &dyn Provider,
    transport: &TransportId,
    model: ModelId,
) -> anyhow::Result<()> {
    let t = provider.transport(transport).context("unknown transport")?;
    let cred = broker.credential(&provider.id(), transport).await?;
    let req = TurnRequest::simple(model, "Reply exactly with hello");
    let mut stream = t.stream(req, cred, CancellationToken::new()).await?;
    while let Some(event) = stream.next().await {
        consume(event?);
    }
    Ok(())
}
```

Deliverable:

```text
xlightcli dev probe codex  "hello"
xlightcli dev probe claude "hello"                       # anthropic-api
xlightcli dev probe claude --transport claude-subscription "hello"
xlightcli dev probe agy    "hello"                       # gemini-api
xlightcli dev probe agy    --transport antigravity "hello"
```

Exit: all 5 commands pass with real accounts, no CLI spawned (shim test), redacted fixtures committed, refresh tested (forced expiry).
If an experimental transport is unstable: document it clearly in `docs/providers/`, it **does not** block Phase 1 (the stable transport is sufficient).

### Phase 1 — Single-agent terminal

TUI (ratatui), session, streaming, built-in tools, permissions, `OutputSpool`, SQLite persistence + resume,
`ContextManager` + compaction, provider selection, `init` wizard, `exec` headless, benchmark harness.

Exit: a single agent can complete a real code-editing task on all 3 providers (stable transport) via the **same** agent loop;
can crash mid-turn and resume; first benchmark recorded into `docs/benchmarks/`; no-provider-CLI test passes (green).

### Phase 2 — Capability system

`CommandRegistry`, `ProviderFeaturePack`, capability manifest, `/provider info`, core commands,
`claude.insights` (Compatible), 1–2 commands for codex and agy.

Exit: autocomplete follows the transport; explicit `/provider:cmd`; `Unavailable` displays correctly; every command has a test.

### Phase 3 — MCP

`McpManager`, pool, ToolBridge, separate MCP OAuth/credentials, permissions, config import from Claude/Codex/Agy.

Exit: 3 agents share 1 MCP stdio server (1 process); config import works while the CLI is absent; provider credentials don't leak to MCP (sentinel test).

### Phase 4 — Multi-agent

spawn/send/wait/join/cancel/delegate, parent/child, scheduler, budgets, agent tree UI, orchestration tools.

Exit: 20 agents in 1 process; cancellation follows the tree; budget enforced; the 20-agent benchmark meets the §17 targets.

### Phase 5 — Workspace orchestration

git worktree lifecycle, parallel editing, diff/review/merge in the TUI, artifact exchange between agents.

Exit: 3 writing agents edit in parallel without colliding; merge requires confirmation; worktree cleanup is clean.

### Phase 6 — Customization

agent profiles, hooks, keybindings, command aliases, themes, provider overrides, model routing, (evaluate) OS sandbox.
Only then consider WASM plugins.

---

## 20. Initial success criteria (v0.x)

- [ ] Running Codex subscription never spawns the Codex CLI.
- [ ] Running Claude (API key stable; subscription experimental) never spawns the Claude CLI.
- [ ] Running Antigravity/Google (Gemini API stable; Antigravity experimental) never spawns `agy`.
- [ ] Provider picker at init.
- [ ] Core agent loop is fully provider-independent (INV-2 enforced via `xtask check-deps`).
- [ ] `/mcp` belongs to the core runtime.
- [ ] At least one provider command via FeaturePack (`claude.insights`).
- [ ] Multi-agent never maps an agent → an OS process.
- [ ] Old sessions don't keep the whole context in RAM.
- [ ] MCP connections are shared.
- [ ] Provider credentials never leak to agent/tool/MCP/hooks (sentinel test).
- [ ] Memory benchmark is reproducible (`cargo xtask bench-mem`).
- [ ] The official provider binary is never a runtime dependency (shim test in CI).
- [ ] Experimental transports are disabled by default and have a kill switch.

---

## 21. Out of scope (v0.x)

100 providers · desktop GUI · mobile · browser IDE · remote/cloud orchestration · daemon mode ·
provider account pooling · quota bypass · full compatibility with every slash command ·
plugin marketplace · Rust dylib plugins · Windows (D-003) · outbound telemetry.

The architecture allows adding later without changing the orchestration core: direct Gemini API (already present), OpenRouter,
local models, Kimi, Grok, …

---

## 22. Risk register

| ID | Risk | Impact | Mitigation |
|----|--------|----------|-----------|
| R-1 | Upstream changes/locks the subscription protocol | Transport dies | Version gating, kill switch, API-key fallback, core doesn't depend on it |
| R-2 | Using a subscription outside the official client violates the ToS → account gets banned | User loses their account | D-002 experimental gate + clear warning; not enabled by default |
| R-3 | Refresh-token rotation conflicts with the official CLI | The official CLI gets logged out | D-017, warning shown on import |
| R-4 | No keyring on headless Linux | Credentials can't be stored | Opt-in file fallback 0600 (D-019), or API key via env |
| R-5 | Porting code from OpenCodex without attribution | Violates the MIT license | D-018: MIT notice + commit SHA in `THIRD_PARTY.md` + file header |
| R-9 | A dependency has a license incompatible with GPLv3 (e.g. the old OpenSSL/SSLeay license) | Can't distribute a valid binary | D-028 greatly reduces the risk; `cargo deny check licenses` still runs in CI; TLS uses rustls + `aws-lc-rs` (checked 2026-09-28: `aws-lc-sys` = ISC/Apache-2.0/MIT/BSD-3-Clause, no OpenSSL license) |
| R-10 | The `antigravity` transport requires spoofing the official client's User-Agent (`antigravity/ide/<ver> …`, per OpenCodex); `claude-subscription` is similar | Violates the ToS, account gets banned | Keep D-002 (experimental, opt-in, warning explicitly stating the UA impersonation) |
| R-11 | Multi-agent recipes (`/boost`, `/teamwork`, `/goal`) consume a lot of quota | User unexpectedly runs out of quota | Tight default budget, shows an estimate + requires confirmation before running, `/usage` always available |
| R-6 | Compaction loses important information | Agent quality degrades | Keep recent turns, artifact refs, regression tests with sample transcripts |
| R-7 | Memory target missed due to allocator fragmentation | Violates the target | Measure early, try mimalloc/jemalloc, an arena for the stream buffer |
| R-8 | Malicious project config (hooks/MCP) | Runs unwanted code | Workspace trust §12.3 |

---

## 23. Open questions

| ID | Question | Status |
|----|---------|-----------|
| Q-1 | Which repo is OpenCodex, what license? | **Resolved** → D-018 (lidge-jun/opencodex, MIT) |
| Q-2 | File fallback secret store on headless Linux? | **Resolved** → D-019 (yes, opt-in) |
| Q-3 | Import & own, or read-through? | **Resolved** → D-017 (import & own) |
| Q-4 | How are agy/Claude/Codex commands inherited? | **Resolved** → D-023, inventory in `docs/commands.md` |
| Q-5 | What does `claude.insights` output? | **Resolved** → D-021 (HTML file, returns the path) |
| Q-6 | License? | **Resolved** → D-028 (GPL-3.0-only; D-020 superseded) |
| Q-7 | Distribution? | **Resolved** → D-022 |
| Q-8 | ~~`GPL-2.0-only` or `GPL-2.0-or-later`?~~ `-or-later` allows distributing the binary under GPLv3, which in turn allows using Apache-2.0 dependencies (very common in the Rust ecosystem, including the TLS/crypto stack), and allows porting code from `openai/codex` (Apache-2.0) — which `-only` would not allow. | **Resolved** → D-028 (GPL-3.0-only) |
