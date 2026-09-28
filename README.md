<h1 align="center">xlightcli</h1>

<h3 align="center">one terminal, your subscriptions, many agents</h3>
<p align="center"><b>A lightweight, provider-aware coding-agent terminal for Codex, Claude and Antigravity</b><br>
Talks to each provider directly — never wraps or spawns <code>codex</code>, <code>claude</code> or <code>agy</code>.</p>

<p align="center">
  <img src="https://img.shields.io/badge/license-GPL--3.0--only-blue" alt="license GPL-3.0-only">
  <img src="https://img.shields.io/badge/rust-1.90%2B-orange?logo=rust" alt="rust 1.90+">
  <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS-24292f" alt="Linux | macOS">
  <img src="https://img.shields.io/badge/status-Phase%200%20(pre--alpha)-lightgrey" alt="status: Phase 0">
</p>

```bash
cargo install --locked --path crates/app      # from a clone of this repository
xlightcli auth login codex                    # ChatGPT sign-in in your browser
xlightcli dev probe codex "Reply exactly with hello"
```

> [!IMPORTANT]
> **xlightcli is in Phase 0** (protocol/auth spike). What works today: provider login, credential
> storage, and streaming a single prompt with `xlightcli dev probe`. The interactive TUI, tools,
> MCP and multi-agent runtime arrive in later phases — see the [roadmap](#roadmap).
> Provider endpoints are implemented from source research and are still being **verified against
> live accounts** ([`docs/providers/`](docs/providers)).

## Why

Every official coding CLI is a full runtime of its own. Run a few agents in parallel and you pay for
N copies of that runtime. xlightcli is built the other way around:

- **One process, many agents.** Agents are async tasks, not OS processes. Memory should grow with the
  active workload, not with the number of agents.
- **Your existing subscriptions.** Sign in with ChatGPT, or reuse an existing Codex/Claude login.
  xlightcli imports the credential and manages it from then on.
- **No provider CLI in the runtime path.** xlightcli owns auth, the wire protocol, tools, MCP and the
  UI. `codex`, `claude` and `agy` can be missing from your machine entirely, and CI checks that none of
  them is ever executed.
- **Familiar commands.** Slash commands from agy, Claude Code and Codex are inherited under their
  upstream names (see [`docs/commands.md`](docs/commands.md)), so switching doesn't break your habits.

## Quick start

### 1. Install

xlightcli is not published to crates.io yet. Build it from source (requires Rust **1.90+**, installed
via [rustup](https://rustup.rs); the repository pins the stable toolchain in `rust-toolchain.toml`):

```bash
git clone <this-repository-url> xlight-cli
cd xlight-cli
cargo install --locked --path crates/app
xlightcli --version
```

The build needs a C compiler (`cc`/`clang`, e.g. `build-essential` or the Xcode Command Line Tools) because
SQLite and the TLS crypto backend (rustls + aws-lc-rs) are compiled from source. No system libraries are required.
On Linux the default credential store talks to the Secret Service over D-Bus (GNOME Keyring,
KWallet, KeePassXC…); without one, use the file store (see [Credentials & security](#credentials--security)).

<details>
<summary><b>Build with the experimental subscription transports</b></summary>

The Claude-subscription and Antigravity transports are compiled out by default (see
[Experimental transports](#experimental-transports) before enabling them):

```bash
cargo install --locked --path crates/app --features experimental
# or individually: --features claude-subscription / --features antigravity-subscription
```

</details>

### 2. Log in

```bash
xlightcli auth list                           # stored accounts + credentials found in other CLIs
xlightcli auth login codex                    # ChatGPT OAuth in the browser
xlightcli auth login codex --method device    # device code, for SSH / headless machines
xlightcli auth login claude                   # Anthropic API key (prompted, never echoed)
xlightcli auth login agy                      # Gemini API key
xlightcli auth import codex                   # reuse an existing ~/.codex login (import & own)
```

Without `--method`, the method follows the transport: API-key transports prompt for a key and
subscription transports open the browser. Without `--transport`, xlightcli picks the provider's first
transport, or the first one that matches `--method`. A method that doesn't fit the transport is
rejected, so an OAuth token can never end up stored under an API-key transport.

If the browser login reports that the callback port is already in use, another process holds it,
often a login left running by another CLI (`ss -ltnp 'sport = :1455'` shows which one for Codex).
Stop that process, or use `--method device`.

API keys can also come from the environment: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`.

### 3. Send a prompt

```bash
xlightcli dev probe codex "Reply exactly with hello"
xlightcli dev probe claude --model <model-id> "Explain Rust lifetimes in one paragraph"
xlightcli dev probe agy "hello"
```

`dev probe` streams text to stdout, reasoning (dimmed) to stderr, and ends with token usage and any
rate-limit information the provider returned. Exit codes: `0` ok · `1` error · `2` invalid input
(unknown provider, not logged in…) · `3` error after partial output.

## Supported platforms

| OS | Status | Credential store |
|---|---|---|
| Linux (x86_64 / aarch64) | Supported | Secret Service (default) or `0600` file store |
| macOS (Apple Silicon / Intel) | Supported | macOS Keychain (default) or `0600` file store |
| Windows | Not yet (planned after v0.x) | — |

## Providers & transports

Each provider is split into **auth**, one or more **transports** (wire protocols), and a **feature
pack** (provider-specific commands). Stable transports use officially supported access paths.

| Provider | Transport | Auth | Stability |
|---|---|---|---|
| `codex` | `chatgpt` | ChatGPT sign-in (browser PKCE or device code), or import `~/.codex/auth.json` | Stable |
| `codex` | `openai-api` | `OPENAI_API_KEY` | Stable |
| `claude` | `anthropic-api` | `ANTHROPIC_API_KEY` | Stable |
| `claude` | `claude-subscription` | Claude account OAuth, or import `~/.claude/.credentials.json` | **Experimental** |
| `agy` | `gemini-api` | `GEMINI_API_KEY` | Stable |
| `agy` | `antigravity` | Google account OAuth (Antigravity / Cloud Code Assist) | **Experimental** |

Pick a transport with `--transport`. Inspect what a provider offers with
`xlightcli provider info <provider>`, which lists transports, capabilities and commands, each marked
Native, Core, Compatible or Unsupported.

### Experimental transports

> [!WARNING]
> `claude-subscription` and `antigravity` use consumer subscriptions outside the official clients.
> To work at all, they have to present themselves like the official client (OAuth client id, request
> headers / User-Agent). Provider terms may prohibit this, and accounts have reportedly been
> restricted for it. They are **off by default**, and you enable them at your own risk. For a
> supported path, use `anthropic-api` or `gemini-api`.

Enabling one takes two separate opt-ins:

1. Compile it in with `--features claude-subscription` or `--features antigravity-subscription`.
2. Opt in at runtime with `XLIGHTCLI_EXPERIMENTAL_CLAUDE_SUBSCRIPTION=1` or
   `XLIGHTCLI_EXPERIMENTAL_ANTIGRAVITY=1`. Until you do, `auth login` and `auth import` refuse to
   sign in to the transport, and every model request fails with `TransportDisabled`.

```bash
XLIGHTCLI_EXPERIMENTAL_CLAUDE_SUBSCRIPTION=1 xlightcli auth login claude --method browser
XLIGHTCLI_EXPERIMENTAL_CLAUDE_SUBSCRIPTION=1 xlightcli dev probe claude --transport claude-subscription "hello"
```

`antigravity` additionally needs the Antigravity desktop OAuth client, which is not shipped in this
repo: set `XLIGHTCLI_ANTIGRAVITY_OAUTH_CLIENT_ID` and `XLIGHTCLI_ANTIGRAVITY_OAUTH_CLIENT_SECRET`
(see `docs/providers/agy.md`, "OAuth client").

In Phase 0 these flags are environment variables. They move to the global config file in Phase 1.

xlightcli does not implement quota bypass, account pooling or rotation, and it never fakes a
provider feature it can't reach.

## Credentials & security

- **Storage.** By default secrets go to the OS keyring (service `xlightcli`). On machines without a
  keyring, set `XLIGHTCLI_AUTH_STORE=file` to use `~/.local/share/xlightcli/credentials/` instead
  (directory `0700`, files `0600`, written atomically; group- or world-readable files are refused).
- **Import & own.** `auth import` copies an existing login into xlightcli's own store and refreshes
  it from then on. It never writes back to the other CLI's files. ChatGPT refresh tokens rotate, so
  after an import the official Codex CLI may ask you to sign in again.
- **Metadata only in SQLite.** `~/.local/share/xlightcli/xlightcli.db` holds account metadata and never
  any secret.
- **No leaks by construction.** Tokens never reach logs, argv, child-process environments, MCP servers
  or hooks. Transports only receive an opaque `CredentialHandle`, and logs pass through a redaction
  layer. A sentinel-secret test enforces this.
- **Single-flight refresh.** When many requests share one account and the token needs refreshing,
  exactly one refresh call goes upstream.

## CLI

```text
xlightcli [-v] <command>

  auth list                                        stored accounts + importable credentials
  auth login  <provider> [--transport T] [--method browser|device|api-key]   (method defaults per transport)
  auth logout <provider> [--transport T] [--account ID]
  auth import <provider>                           reuse a login found in another CLI's config
  provider list                                    providers and their transports
  provider info <provider>                         capabilities, transports, commands
  dev probe <provider> [--transport T] [--model M] <prompt>
  dev models <provider> [--transport T]          models available to the logged-in account
  dev quota  <provider> [--transport T]          plan/quota snapshot, when the provider exposes it
```

`-v` also prints debug logs to stderr. Logs always go, redacted, to
`~/.local/state/xlightcli/logs/xlightcli.log`.

| Environment variable | Purpose |
|---|---|
| `XLIGHTCLI_AUTH_STORE` | `keyring` (default) or `file` |
| `XLIGHTCLI_EXPERIMENTAL_CLAUDE_SUBSCRIPTION` / `XLIGHTCLI_EXPERIMENTAL_ANTIGRAVITY` | runtime opt-in for experimental transports (`1`) |
| `XLIGHTCLI_API_KEY_<PROVIDER>` | non-interactive API key for `auth login --method api-key` |
| `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY` | provider API keys |
| `CODEX_HOME` | where to look for an existing Codex login (default `~/.codex`) |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` | relocate xlightcli's directories (honored on macOS too) |

### Files

```text
~/.config/xlightcli/config.toml        global config (Phase 1)
~/.local/share/xlightcli/xlightcli.db  account metadata (SQLite, WAL)
~/.local/share/xlightcli/credentials/  file credential store (only with XLIGHTCLI_AUTH_STORE=file)
~/.local/state/xlightcli/logs/         redacted logs
<repo>/.xlightcli/                     project config (Phase 1)
```

### Uninstall

```bash
xlightcli auth logout <provider>       # for each account; removes the keyring entry and the metadata
cargo uninstall xlightcli
rm -rf ~/.config/xlightcli ~/.local/share/xlightcli ~/.local/state/xlightcli
```

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| 0 | Protocol/auth spike: login → refresh → stream for all three providers, without the provider CLIs | **in progress**: implemented, live verification pending |
| 1 | Single-agent TUI: sessions, tools, permissions, SQLite persistence, `exec` headless mode | planned |
| 2 | Capability system: command registry, feature packs, `/insights`, core recipes (`/plan`, `/goal`, `/btw`…) | planned |
| 3 | Shared MCP manager and config import from other CLIs | planned |
| 4 | Multi-agent runtime: spawn/wait/cancel, scheduler, budgets | planned |
| 5 | Workspace orchestration with git worktrees | planned |
| 6 | Customization: profiles, hooks, skills, keybindings, themes | planned |

Details and exit criteria: [`docs/PLAN.md`](docs/PLAN.md) §19.

## Documentation

| Document | Contents |
|---|---|
| [`docs/PLAN.md`](docs/PLAN.md) | Architecture, invariants, decision log, phases, risks |
| [`CODEBASE.md`](CODEBASE.md) | Crate map, allowed dependency edges, data flow |
| [`PATTERNS.md`](PATTERNS.md) | Required code patterns (errors, async, credentials, adapters, tests) |
| [`docs/CONTRACTS.md`](docs/CONTRACTS.md) | Public API of `protocol`, `auth`, `provider`, `config` |
| [`docs/commands.md`](docs/commands.md) | Slash-command inventory inherited from agy / Claude Code / Codex |
| [`docs/import.md`](docs/import.md) | How other CLIs' config, rules, skills, hooks and MCP map into xlightcli |
| [`docs/providers/`](docs/providers) | Per-provider research notes and live-verification checklists |
| [`AGENTS.md`](AGENTS.md) | Rules for coding agents (and humans) working in this repo |

## Development

```bash
cargo build --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo xtask check-deps          # enforce the crate dependency rules (CODEBASE.md §3)
cargo xtask no-provider-cli     # run the CLI with fake codex/claude/agy shims on PATH; fail if any runs
cargo xtask redact-fixture <f>  # scrub tokens/ids from a recorded fixture before committing it
```

Tests use synthetic fixtures and local mock servers, and never contact a real provider. Checks against
real accounts are manual. Log in, run `xlightcli dev probe` for each transport, and work through the
"Live verification checklist" in [`docs/providers/<provider>.md`](docs/providers). Any fixture you
record from a live session must go through `cargo xtask redact-fixture` before it is committed.

Read [`AGENTS.md`](AGENTS.md) before contributing. It covers the invariants, the phase gate, the
conventions (English everywhere, SPDX headers, Conventional Commits) and what to check before calling
something done.

## Disclaimer

xlightcli is an independent project. It is **not affiliated with or endorsed by OpenAI, Anthropic or
Google**. It uses only the account you sign in with, and you are responsible for complying with each
provider's current terms. The experimental subscription transports may break or be restricted by
upstream at any time. Use them at your own risk.

Parts of the provider auth and wire code are ported from
[OpenCodex](https://github.com/lidge-jun/opencodex) (MIT); see [`THIRD_PARTY.md`](THIRD_PARTY.md).

## License

[GPL-3.0-only](LICENSE)
