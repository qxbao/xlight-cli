# AGENTS.md — xlightcli

Standard guide for **every coding agent** (Claude Code, Codex, Gemini/Agy, …) working in this repo.
`CLAUDE.md` imports this file; it is the single source of truth for working rules.

## 1. What this project is

`xlightcli` is a coding-agent terminal written in Rust: it manages its own auth, calls the
Codex / Claude / Antigravity (agy) backends directly, runs tools, MCP, multi-agent — all in
**one process**. It does **not** wrap or spawn the provider's official CLI.

| Document | Content |
|----------|---------|
| [`docs/PLAN.md`](docs/PLAN.md) | Technical plan, invariants (§2), decision log (§3), phases (§19), open questions (§23) |
| [`CODEBASE.md`](CODEBASE.md) | Crate map, dependency rules, data flow, implementation status |
| [`PATTERNS.md`](PATTERNS.md) | Mandatory code patterns (error, async, channel, credential, adapter, test) |
| [`docs/commands.md`](docs/commands.md) | Inventory of agy / Claude Code / Codex slash commands → xlightcli (mode, phase) |
| [`docs/import.md`](docs/import.md) | Mapping of config/credential/rules/skills/hooks/MCP from other CLIs → xlightcli |
| `docs/providers/<provider>.md` | Research notes + wire/auth spike results for each provider |
| [`docs/CONTRACTS.md`](docs/CONTRACTS.md) | Public API summary (protocol/auth/provider/config) |

**Current status:** Phase 0 not started — no code yet. When scaffolding, follow the layout in `CODEBASE.md`.

## 2. Invariants — MUST NOT be violated

Details and how they're enforced: `docs/PLAN.md` §2. Summary:

1. **Never spawn a provider CLI** (`codex`, `claude`, `agy`, `antigravity`) anywhere in the runtime path. Every process spawn goes through `ProcessLauncher`.
2. **Provider-independent runtime**: `runtime`, `tui` must not depend on `provider-*`; no `match` on provider id in the agent loop. Only `app` knows about concrete providers.
3. **Canonical protocol**: core only ever sees `TurnRequest` / `AgentEvent` / `Message`. A provider's wire type is `pub(crate)` inside the adapter crate.
4. **Credentials never leave the `auth` crate**: use `CredentialHandle`. Never log, never store as plaintext, never put into argv/env/MCP/hooks.
5. **Agent = async task**, not an OS process.
6. **Bounded channel / pull stream** for data. Never `unbounded_channel()`.
7. **Tool output via `OutputSpool`** (head + tail + artifact file), never buffer the whole thing.
8. **MCP belongs to core**, one server spawned once, shared across agents.
9. **No** quota bypass, account rotation/pooling, or identity spoofing outside the experimental transport that the user has explicitly opted into.
10. Unsupported feature ⇒ return a clear `Unavailable`, **never fake it**.
11. Adapter errors must never crash the runtime.

If a request forces you to violate an invariant: **stop and ask the user**, don't look for a workaround.

## 3. Common commands

> The commands below take effect once the workspace has been scaffolded (Phase 0).

```bash
cargo build --workspace
cargo test --workspace                        # or: cargo nextest run --workspace
cargo test -p xlightcli-provider-codex        # test a single crate
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo xtask check-deps                        # enforce dependency rules (INV-2)
cargo xtask no-provider-cli                   # e2e with shim binaries (INV-1)
cargo xtask bench-mem                         # benchmark memory with a mock provider
cargo xtask redact-fixture <file>             # REQUIRED before committing a fixture
cargo run -p xlightcli -- dev probe codex "hello"

# Live test with a real account — run manually, never in CI
XLIGHTCLI_LIVE_CODEX=1 cargo test -p xlightcli-provider-codex -- --ignored live_
```

Verify with raw output: if your shell wraps cargo output (e.g. rtk), use the unfiltered command and
check the exit code — filtered output has hidden clippy errors before.

Before reporting "done": `cargo fmt --all`, `cargo clippy ... -D warnings`, `cargo test` for the affected
crates, and `cargo xtask check-deps` must all pass. If you couldn't run a command, say clearly which one and why.

## 4. Workflow

1. **Read before editing**: identify the relevant crate in `CODEBASE.md`, read the matching pattern in `PATTERNS.md`.
2. **Cross-crate changes or touching a public trait** (`Provider`, `TransportAdapter`, `AuthAdapter`, `Tool`, `ProviderFeaturePack`, a type in `protocol`): describe the design first, update `docs/PLAN.md` / `CODEBASE.md` in the same change.
3. **New architectural decision** ⇒ add a line to the decision log `docs/PLAN.md` §3 (don't delete old lines; mark them *superseded*).
4. **Follow the phases**: don't implement a feature from a later phase before the current phase meets its exit criteria (`docs/PLAN.md` §19), unless the user asks for it.
5. **New code must have tests** at the appropriate layer (`docs/PLAN.md` §16). Adapter: fixture + snapshot. Runtime: `MockProvider`.
6. **Don't add heavy dependencies** (runtime, TLS, DB, another async executor) without stating a reason; prefer a crate already in the workspace. Declare the version in `[workspace.dependencies]`.
7. Update the "Status" column in `CODEBASE.md` §2 whenever a crate/module changes status.

## 5. Conventions

- License **GPL-3.0-only** (D-028). Every `.rs` file starts with `// SPDX-License-Identifier: GPL-3.0-only`. New dependencies must be GPLv3-compatible (`cargo deny check licenses`).
- Rust edition 2024. Code, identifiers, comments, commit messages, AND all documentation (root `*.md`, `docs/**`) are in **English** (D-029, supersedes the docs part of D-015).
- Commit: Conventional Commits, scope is the crate name — e.g. `feat(provider-codex): stream reasoning deltas`.
- Error: `thiserror` in library crates; `anyhow` only in `app` and `xtask`.
- No `unwrap()` / `expect()` outside tests, except for a genuine invariant (with a comment explaining why).
- No `println!` in library crates — use `tracing`. User-facing output goes through the TUI / `exec` renderer.
- Comments explain **why**, not restate the code.

## 6. Provider & auth — special notes

- **Experimental** transports (`claude-subscription`, `antigravity`) sit behind a cargo feature + runtime opt-in (`docs/PLAN.md` §15). Don't make them default-on, don't skip the acknowledgement step.
- Endpoint/header/OAuth client info in `docs/providers/*.md` must be tagged with a confidence label (H/M/U) and a date. Only add it to code (`consts.rs`) after a spike has verified it (label **verified** + date). Don't guess and hard-code.
- **Reading** the official CLI's credentials/config (discovery/import) is allowed. **Never** write back into their files, **never** call their binary.
- SSE/JSON fixtures must be redacted (token, account id, email, org id) via `cargo xtask redact-fixture`.
- Ported from **OpenCodex** (<https://github.com/lidge-jun/opencodex>, MIT, TypeScript): only port modules listed in the allowed table in `docs/PLAN.md` §4.5. **Do not** port account pooling/failover, the `claude-cli` adapter, intercept/local-CA, proxy/dashboard. Every ported file gets a header `Portions derived from OpenCodex (MIT) @ <commit-sha>: src/<path>` and a line added to `THIRD_PARTY.md`.
- New commands / commands ported from agy, Claude Code, Codex: keep the upstream name + aliases, update `docs/commands.md` (D-023).

## 7. Things NOT to do unless the user explicitly asked

- Commit / push / open a PR.
- Run a live test with a real account or send a request to a real provider.
- Enable an experimental transport in default features / default config.
- Delete or rewrite the decision log or the invariants.
- Read the user's real credential files (`~/.codex`, `~/.claude`, keychain, …) outside the designed discovery flow.
