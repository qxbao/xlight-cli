# CODEBASE.md — xlightcli codebase map

> Describes the **target layout** of the Cargo workspace and the current status of each part.
> When code changes the structure (add/remove/rename a crate, module, dependency edge), update this file in the same change.
> Design rationale lives in [`docs/PLAN.md`](docs/PLAN.md); code patterns live in [`PATTERNS.md`](PATTERNS.md).

## 1. Directory tree

```text
xlight-cli/
├── Cargo.toml                 [workspace] + [workspace.dependencies] + [workspace.lints]
├── rust-toolchain.toml        pin stable toolchain
├── clippy.toml                disallowed-methods (Command::new, unbounded_channel, ...)
├── deny.toml                  cargo-deny: license + advisories
├── .cargo/config.toml         alias `xtask = "run -p xtask --"`
├── LICENSE                    GPL-3.0 (D-028)
├── AGENTS.md · CLAUDE.md · CODEBASE.md · PATTERNS.md · THIRD_PARTY.md
├── docs/
│   ├── PLAN.md
│   ├── commands.md            inventory of agy / Claude Code / Codex commands → xlightcli (D-023)
│   ├── providers/             codex.md · claude.md · agy.md  (Phase 0 spike results)
│   └── benchmarks/             bench-mem results by date
├── crates/
│   ├── protocol/              xlightcli-protocol
│   ├── config/                xlightcli-config
│   ├── storage/               xlightcli-storage
│   ├── auth/                  xlightcli-auth
│   ├── provider/              xlightcli-provider
│   ├── provider-codex/        xlightcli-provider-codex
│   ├── provider-claude/       xlightcli-provider-claude
│   ├── provider-agy/          xlightcli-provider-agy
│   ├── tools/                 xlightcli-tools
│   ├── mcp/                   xlightcli-mcp
│   ├── runtime/               xlightcli-runtime
│   ├── tui/                   xlightcli-tui
│   └── app/                   xlightcli  (binary)
├── xtask/                     check-deps · no-provider-cli · bench-mem · redact-fixture
└── tests/                     (if needed) shared e2e; main e2e lives in crates/app/tests
```

## 2. Crates

Status: `planned` → `scaffolded` → `in-progress` → `usable` → `stable`.

| Crate | Responsibility | Main modules | Phase | Status |
|-------|-------------|--------------|-------|-----------|
| `protocol` | Canonical types, no IO: `TurnRequest`, `Message`, `ContentBlock`, `OpaqueBlob`, `AgentEvent`, `StopReason`, `Usage`, `ToolDefinition`, `ProviderCapabilities`, `CapabilityMode`, `ProviderError`, the IDs (`ProviderId`, `TransportId`, `ModelId`, `SessionId`, `AgentId`, `ToolCallId`, `CommandId`, `WorkspaceId`). | `ids`, `message`, `event`, `tool`, `capability`, `error`, `usage` | 0 | **usable** (complete, serde roundtrip tested; `WorkspaceId` added in Phase 1 Wave A for `storage`'s `workspaces` table, docs/PLAN.md §11.2) |
| `config` | Load + merge multi-layer config, schema, XDG paths, workspace trust, `config show --origin`. | `paths`, `flags`, `schema`, `partial`, `merge`, `origin`, `env`, `trust`, `loader` | 0–1 | **usable** (Phase 1 Wave A: full schema, `PartialConfig` per layer, deterministic `merge`/`resolve`, `Origin` tracking at section granularity, minimal `env` allowlist, `TrustStore`, `ConfigLoader` orchestrating the full layer order — all real, tested; not yet wired into `app`) |
| `storage` | SQLite WAL, migrations, persistence worker (writer thread), event log, projections, artifact store, `AccountIndex` (metadata, no secrets). | `db`, `migrations`, `worker`, `records`, `storage`, `accounts` | 1 (0: only `accounts`) | **usable** (Phase 1 Wave A: `Storage` — writer thread + bounded `StorageCmd` channel, migration `0002_sessions.sql` (workspaces/sessions/agents/events/messages/tool_calls/artifacts/summaries/provider_usage), full CRUD/append/paged-read API — implemented + tested; `messages` is populated by explicit caller writes rather than auto-derived from `events`, a documented Wave A scope decision) |
| `auth` | `AuthBroker`, `AuthAdapter` trait, `CredentialHandle`, `SecretStore` (keyring / opt-in file), `RefreshCoordinator` (single-flight), OAuth toolkit (PKCE, loopback, device code), discovery helpers, redaction patterns. | `broker`, `adapter`, `handle`, `store`, `refresh`, `oauth`, `discovery`, `redact` | 0 | **usable** (Wave 2: `KeyringStore`/`FileStore`, single-flight `RefreshCoordinator` + generation counter in `CredentialHandle`, `AuthBroker::credential/login/import/accounts/logout` with `AccountIndex` cache, `LoopbackServer` + device-code + token exchange/refresh, tracing redaction `MakeWriter` — all implemented + tested) |
| `provider` | `Provider`, `TransportAdapter`, `ProviderFeaturePack`, `ConfigImporter` traits; `ProviderRegistry`; shared infrastructure: HTTP client factory, SSE parser, retry/backoff, rate-limit parsing, kill switch, fixture/snapshot test harness, `MockProvider` (feature `testing`). | `traits`, `registry`, `http`, `sse`, `retry`, `gate`, `testing` | 0 | **usable** (complete, well tested, no need to change when implementing an adapter) |
| `provider-codex` | Transport `chatgpt` (stable), `openai-api` (stable, optional); ChatGPT OAuth auth + discovery; Responses wire; `CodexFeaturePack`; `~/.codex` importer. | `auth`, `wire`, `transport_chatgpt`, `transport_api`, `transport_common`, `quota`, `import`, `features`, `consts` | 0 | **usable** — `impl Provider`/`TransportAdapter` for both transports; wire request/response translator (fixture + insta snapshot tested: text, reasoning+encrypted, tool call, parallel tool calls, error mid-stream, rate-limit); auth (ChatGPT OAuth browser PKCE + non-RFC-8628 device-code grant, both wiremock-tested end to end; API key; `~/.codex/auth.json` discover/import; refresh); `x-codex-primary-*`/`codex.rate_limits` rate-limit parsing; `/wham/usage` quota (best-effort parse, **U**); `~/.codex/config.toml` `[mcp_servers.*]` importer. `codex.*` FeaturePack commands still empty (Phase 2+, `docs/commands.md` §3.3). Known gaps: base URL / `/usage` path / originator header name still **M/U** — see `docs/providers/codex.md` "Live verification checklist" |
| `provider-claude` | Transport `anthropic-api` (stable), `claude-subscription` (**experimental**, cargo feature `claude-subscription`); Messages wire; `ClaudeFeaturePack` (`insights`); `~/.claude` importer. | `auth`, `wire`, `transport_api`, `transport_subscription`, `quota`, `import`, `features/insights` | 0 | **usable** — `impl Provider`/`TransportAdapter` for both transports; wire request/response translator (fixture + insta snapshot tested); auth (API key + OAuth login/refresh/discovery/import, `claude-subscription` feature-gated); rate-limit/quota header parsing; `.mcp.json`/`~/.claude.json` importer. `claude.insights` still `Unavailable` (needs Phase 2 session storage). Known gap: adaptive-thinking wire for newer model families not implemented — see `docs/providers/claude.md` |
| `provider-agy` | Transport `gemini-api` (stable), `antigravity` (**experimental**, cargo feature `antigravity-subscription`); Gemini/Cloud Code Assist wire; `AgyFeaturePack`; `~/.gemini`, `.agents/` importer. | `auth`, `wire`, `transport_gemini`, `transport_antigravity`, `quota`, `import`, `features`, `consts` | 0 | **usable** — `impl Provider`/`TransportAdapter` for both transports; wire request/response translator (fixture + insta snapshot tested, incl. Cloud Code Assist `{response:{...}}` wrapper); auth (API key incl. `GEMINI_API_KEY` env, Google OAuth PKCE login/refresh/revoke, `antigravity-subscription` feature-gated); quota (`retrieveUserQuotaSummary` → `fetchAvailableModels` fallback); `.gemini/.agents` MCP config importer. Wave 3: `ContentBlock::ToolUse.opaque` (protocol addition) now carries per-functionCall `thoughtSignature`; `AccountInfo.metadata` (auth addition) holds the discovered Cloud Code Assist project id (`AgyEndpoints::antigravity_project_id` kept as an explicit override only); `oauth::{exchange_code_for_token,refresh_access_token}_with_secret` (additive) used for the OAuth client's `client_secret`. Known gaps (see `docs/providers/agy.md`): `AuthBroker::run_refresh` doesn't yet propagate a refreshed `AccountInfo.metadata` into the persisted index (login-time only); no live account verification. All `agy.*` FeaturePack commands still `Unavailable` (Phase 2+). |
| `tools` | `Tool` trait, `ToolRegistry`, built-in tools, `PermissionEngine`, `ProcessLauncher` (the single spawn point), `OutputSpool`. | `registry`, `builtin/{fs,grep,shell,git}`, `permission`, `launcher`, `spool`, `tool` | 1 | **in-progress** (Phase 1 Wave A: `Tool`/`ToolContext`/`ToolRegistry`, `PermissionEngine`/`PermissionGate` (real rule evaluation, deny>ask>allow, glob + `regex:`), `ProcessLauncher`/`OutputSpool` (real, process-group kill on timeout/cancel, RAM-bounded spooling) — all implemented + tested. The 9 built-in tool structs are registered with real `ToolDefinition`s but every `run` body is `ToolError::NotImplemented` — Wave B) |
| `mcp` | `McpManager`, `ConfigRegistry` (+ importer), `ConnectionPool`, `ToolBridge`, MCP credential broker. | `manager`, `config`, `import`, `pool`, `bridge`, `credentials` | 3 | **scaffolded** (empty, doc comments only) |
| `runtime` | Session, `AgentLoop`, `ContextManager` (rules, token estimate, compaction), `ToolExecutor`, `Scheduler` (semaphores, budgets), orchestration (spawn/wait/…), `CommandRegistry` + core commands, **recipes** (plan/goal/btw/review/boost/teamwork… — D-024), execution mode, skills/custom agents/rules loader, artifact review, checkpoint (`/rewind`), in-process scheduler (`/schedule`), `WorkspaceManager` (worktree), hooks, `RuntimeHandle` API for the frontend. | `session`, `agent`, `context`, `commands/{mod,core}`, `exec`, `handle`, `testing` | 1–5 | **in-progress** (Phase 1 Wave A: `RuntimeHandle` — channel/session plumbing, `subscribe`/`create_session`/`resume_session`/`run_command`/`cancel_turn`/`set_execution_mode` — real and tested; `CommandRegistry` + the 14 Phase-1 core command ids; `ContextManager::{estimate_tokens,should_compact}` real; `ExecOptions`/`ExecOutput`/`ExecExitCode` (D-026 contract) real. `AgentLoop::run_turn`, `ContextManager::build_turn_request`, `exec::run_exec` are stubs — Wave B. No `mcp` field in `RuntimeDeps` yet (documented: `xlightcli-mcp` has no public type until Phase 3). `executor`/`scheduler`/`orchestration`/`recipes`/`mode` (as a dedicated module)/`skills`/`rules`/`artifacts`/`checkpoint`/`timers`/`workspace`/`hooks` not started — later phases per the Phase column) |
| `tui` | ratatui + crossterm: prompt, transcript, agent tree, command palette + autocomplete, diff view, permission dialog, status line, init wizard screens. Only talks to `RuntimeHandle`. | `app`, `view/*`, `input`, `keymap`, `theme` | 1 | **in-progress** (Phase 1 Wave A: `run` is a real terminal lifecycle — raw mode + alternate screen, `tokio::select!` over `UiEvent`s and crossterm key events, restores the terminal on exit — but never calls `Terminal::draw` (no rendering yet, by design). `App`/`view::*` (`TranscriptView`, `PromptView`, `StatusLineView`, `PermissionDialogView`, `DiffView`, `CommandPaletteView`)/`Keymap`/`Theme` are real state types, tested where feasible without a TTY. Actual `ratatui` rendering is Wave B) |
| `app` | Binary `xlightcli` (`lib.rs` + thin `main.rs`, for in-process testing): CLI (clap), wiring all crates, **registers the concrete providers** (`CodexProvider`/`ClaudeProvider`/`AgyProvider` + `AuthBroker` via `ProviderAuthAdapter` forwarding), `dev probe`, `auth {list,login,logout,import}`, `provider {list,info}`, `exec`, `init`, bare TUI. `config` subcommand not done yet. | `lib`, `main`, `cli`, `wiring`, `output`, `logging`, `login_ui`, `cmd/{dev,auth,provider,error,exec,init}` | 0–1 | **usable** (Phase 1 Wave A: `exec`'s arg parsing/validation/runtime-wiring is real, its turn execution delegates to `xlightcli_runtime::run_exec` (a Wave B stub, surfaced as a clear exit-code-1 error, never a crash); bare `xlightcli` builds a real `RuntimeHandle` and calls `xlightcli_tui::run`; `init` is a stub reporting a clear error. `wiring::AppContext` (Phase 0 shape) is untouched — `wiring::RuntimeContext`/`build_runtime`/`build_runtime_at` are additive) |
| `xtask` | Dev tooling, not shipped. | `check_deps` (rule engine + `cargo metadata` parser), `no_provider_cli` (shim binaries + temp XDG), `bench_mem` (stub, Phase 1), `redact_fixture` (structural JSON-key + pattern regex, `--check` mode) | 0 | **usable** (`check-deps`/`no-provider-cli`/`redact-fixture` genuinely implemented + tested; `bench-mem` is intentionally still a stub) |

## 3. Dependency rules

Allowed edges (A → B means A may depend on B). Any other edge is rejected by `cargo xtask check-deps`.

```text
protocol        → (no internal crate)
config          → protocol
storage         → protocol
auth            → protocol, config, storage
provider        → protocol, auth
provider-*      → provider, protocol, auth, config
tools           → protocol, config, storage
mcp             → protocol, config, tools
runtime         → protocol, config, storage, auth, provider, tools, mcp
tui             → protocol, config, runtime
app             → all of them
xtask           → (no internal crate; reads `cargo metadata`)
```

Important consequences:

- `runtime` and `tui` **never** depend on `provider-codex|claude|agy` (INV-2). Only `app` registers concrete providers into `ProviderRegistry`.
- `tools` and `mcp` **do not** depend on `auth` ⇒ there is no code path that lets a provider credential reach a tool/MCP (INV-4).
- `tui` does not depend on `storage`/`provider` directly — everything goes through `RuntimeHandle`.
- A provider's wire type is `pub(crate)`; only the `Provider`/`TransportAdapter` impl is `pub`.

## 4. Flow of a single turn

```text
tui ──UserInput──▶ RuntimeHandle ──▶ Session/AgentLoop (runtime)
                                        │
            ContextManager.build() ◀────┤  rules + history (storage) + tools (ToolRegistry + McpManager)
                                        │
            Scheduler.llm_permit() ◀────┤  semaphore per (transport, account)
                                        │
            AuthBroker.credential() ◀───┤  → CredentialHandle
                                        ▼
            TransportAdapter.stream(TurnRequest, CredentialHandle, cancel)   (provider-*)
                  │  wire request ─▶ upstream ─▶ SSE
                  │  SSE parser (provider) ─▶ wire translator (provider-*) ─▶ AgentEvent
                  ▼
            EventStream (pull) ──▶ AgentLoop
                                     ├─▶ storage worker (batched events)
                                     ├─▶ UiEvent ─▶ tui (bounded, coalesced deltas)
                                     └─ Completed{stop: ToolUse}
                                           ▼
                                   ToolExecutor ─▶ PermissionEngine ─▶ (tui dialog if ask)
                                           ▼
                                   Tool.run() / MCP ToolBridge ─▶ ProcessLauncher / OutputSpool
                                           ▼
                                   ToolResult ─▶ Message ─▶ back to the loop
```

## 5. Process & task model

```text
xlightcli (1 process, Tokio multi-thread)
├── task: tui event loop            (crossterm events + UiEvent receiver, redraw on change)
├── task/agent: AgentLoop           (1 task per agent; bounded mailbox)
│     └── task: provider stream     (lives for the duration of one turn)
├── task: McpManager + 1 task/connection
├── task: RefreshCoordinator        (proactive refresh)
├── thread: storage writer          (rusqlite, receives commands via bounded channel)
└── child processes (only via ProcessLauncher):
      shell tool commands · MCP stdio servers (each server once) · hooks · git
      NEVER: codex · claude · agy · antigravity
```

Process-wide shared resources (initialized in `app::wiring`, passed down as `Arc`): `reqwest::Client` per
transport, `AuthBroker`, `ToolRegistry`, `McpManager`, `Storage`, `Scheduler`, `ProviderRegistry`.

## 6. On-disk data

```text
~/.config/xlightcli/config.toml
~/.local/share/xlightcli/
├── xlightcli.db (+ -wal, -shm)     schema: docs/PLAN.md §11.2
├── artifacts/<session-id>/<call-id>.log
└── worktrees/<repo-id>/<agent-name>/
~/.local/state/xlightcli/logs/xlightcli.log   (redacted, rotated)
<repo>/.xlightcli/{config.toml, config.local.toml, rules/}
OS keyring: service "xlightcli", account "<provider>:<account-id>"
```

## 7. Where to find common work

| Want to… | See |
|-------|-----|
| Add a field to a canonical event/message | `crates/protocol/src/{event,message}.rs` → update every translator in `provider-*` + snapshots |
| Add a new transport for an existing provider | `crates/provider-<x>/src/transport_*.rs`, register in `Provider::transports()`; PATTERNS.md §5 |
| Add a new provider | new crate `provider-<new>` + register in `crates/app/src/wiring.rs` + edge in xtask `check_deps` |
| Add a built-in tool | `crates/tools/src/builtin/`, register in `ToolRegistry::builtin()`; PATTERNS.md §7 |
| Add a core slash command | `crates/runtime/src/commands/core/` |
| Add a provider slash command | `crates/provider-<x>/src/features/` + `CommandDefinition`; if Compatible, logic lives in `crates/runtime/src/recipes/`, FeaturePack just points to the recipe; update `docs/commands.md` |
| Add a config import source | `crates/provider-<x>/src/import.rs` (`ConfigImporter`) + `docs/import.md` |
| Add a config key | `crates/config/src/schema.rs` + default + merge test + docs/PLAN.md §12 |
| Add a DB table/column | new migration in `crates/storage/src/migrations/` (don't edit an already-merged migration) |
| Spawn a process | Only via `tools::launcher::ProcessLauncher` |
