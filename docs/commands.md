# Command inventory — agy / Claude Code / Codex → xlightcli

> Research date **2026-09-28** (D-023: inherit as much as possible so migration doesn't break workflows).
> Main sources: agy — <https://antigravity.google/docs/cli/reference/>, <https://antigravity.google/docs/slash-commands/>,
> CHANGELOG <https://github.com/google-antigravity/antigravity-cli/blob/main/CHANGELOG.md> (v1.0.0–1.2.12);
> Claude Code — <https://code.claude.com/docs/en/commands.md>; Codex — `openai/codex` `codex-rs/tui/src/slash_command.rs`
> (rust-v0.157.1); OpenCodex (wire/quota).
> Confidence: **H** = present in docs/source · **M** = inferred · **U** = unverified.
> Upstream changes quickly — review this table at the start of every phase.

## 1. Principles

1. **Keep upstream names + aliases unchanged.** Users type the command they're already used to and it runs.
2. **A command present in ≥ 2 CLIs, or provider-independent ⇒ `core.*`**, with aliases being the union of the upstream names.
   Behavior is unified; provider differences go through a capability (e.g. `/effort` maps to the transport's wire field).
3. **A command that exists in only one CLI ⇒ a FeaturePack** of that provider (`agy.*`, `claude.*`, `codex.*`).
4. **A Compatible command is a Recipe** (D-024): a client-side workflow (prompt template + orchestration graph + artifact)
   living in `runtime::recipes`, provider-independent. The FeaturePack only declares the alias + default.
   This lets `/boost` run on Claude or Codex when the user enables `commands.cross_provider = true`
   (invoked as `/agy:boost`). By default, only the active provider's recipe is shown.
5. **No faking it.** Unsupported commands are still registered so that when the user types them they see a clear message
   (`Unavailable: <reason>` + a suggested alternative / URL), instead of an "unknown command" error.
6. **Skills ⇒ slash command** automatically (`/<skill>`, plugin skill `/<plugin>:<skill>`), as in all three CLIs.
7. Alias collisions: core > FeaturePack of the active provider > skill. A shadowed name can still be invoked via `/<ns>:<name>`.

The phase in the last column is the planned implementation phase (`docs/PLAN.md` §19).

## 2. Core commands

| xlightcli id | Alias (upstream union) | Source | Behavior | Mode / notes | Phase |
|--------------|---------------------|-------|---------|----------------|-------|
| `core.help` | `/help`, `?` | agy CC CX | Help: General / Commands / Shortcuts | Core | 1 |
| `core.exit` | `/exit`, `/quit` | all | Exit (confirm if an agent is running) | Core | 1 |
| `core.clear` | `/clear`, `/new` | all | New conversation; the old session can still be resumed | Core | 1 |
| `core.resume` | `/resume`, `/switch`, `/conversation` | all | Session picker by workspace (rename/delete) | Core | 1 |
| `core.rename` | `/rename` | all | Rename session | Core | 1 |
| `core.fork` | `/fork`, `/branch` | all | Clone the conversation into a parallel session | Core | 2 |
| `core.rewind` | `/rewind`, `/undo`, `/checkpoint` | agy CC | Roll back to a previous step (message + file changes) — requires a checkpoint file | Core | 2 |
| `core.compact` | `/compact [instr]` | CC CX | Summarize context | Core (Native compaction if the transport supports it — U) | 1 |
| `core.autocompact` | `/autocompact` | CC | Auto-compact threshold | Core | 2 |
| `core.context` | `/context` | all | Breakdown context usage | Core | 1 |
| `core.model` | `/model [name] [prompt]` | all | Picker; `name prompt` = run one prompt with a different model then return (agy) | Core + catalog Native | 1 |
| `core.effort` | `/effort [low\|medium\|high]` | agy CC CX | Reasoning effort → maps to the transport's wire field/model id | Native mapping | 2 |
| `core.fast` | `/fast` | CC CX (agy removed it in 1.1.0) | Fast tier if the catalog supports it, otherwise = `/effort low` | Native/Compatible | 2 |
| `core.provider` | `/provider [info]` | xlightcli | New session with a different provider; `info` = capability manifest | Core | 2 |
| `core.usage` | `/usage`, `/quota`, `/cost`, `/stats` | all | Quota/plan bars (Native from the transport) + cost/stats computed locally | Native + Core | 2 |
| `core.status` | `/status` | CC CX | Version, model, account, transport, connectivity, token usage | Core | 1 |
| `core.permissions` | `/permissions`, `/approve` | all | Allow/ask/deny rules by scope; `approve` = re-approve a previously denied one-off | Core | 1 |
| `core.mode` | Shift+Tab, `/mode` | agy CC CX | Cycle execution mode `default → accept-edits → plan` | Core | 1 |
| `core.plan` | `/plan [prompt]` | all | Plan mode: read-only tools → an "Implementation Plan" artifact for review/approval | Compatible recipe | 2 |
| `core.goal` | `/goal <objective>` | all | Automatic loop until the objective is met (still subject to budget + permissions) | Compatible recipe | 2 |
| `core.btw` | `/btw <q>`, `/side` | all | Ephemeral side thread, no tools, doesn't interrupt the main agent | Compatible recipe | 2 |
| `core.recap` | `/recap` | CC CX | Summarize what was done in the session | Compatible recipe | 2 |
| `core.review` | `/review`, `/code-review` | CC CX | Review diff/working tree/PR → findings | Compatible recipe (CC's `ultra` level = cloud ⇒ Unsupported) | 2 |
| `core.security-review` | `/security-review` | CC | Security review of the branch against the default branch | Compatible recipe | 2 |
| `core.init` | `/init` | CC CX | Generate `AGENTS.md` (and a `CLAUDE.md` that imports `@AGENTS.md` if the user chooses) | Compatible recipe | 2 |
| `core.memory` | `/memory`, `/memories` | CC CX | Open/edit rules file + memory | Core | 2 |
| `core.learn` | `/learn [instr]` | agy | Distill an in-session correction into a rule/skill (written to `.agents/rules/` or `.agents/skills/`) | Compatible recipe | 2 |
| `core.diff` | `/diff` | all | Diff viewer (including untracked) | Core | 1 |
| `core.artifact` | `/artifact`, Ctrl+R | agy | Artifact review panel (y/n, approve all, line comments) | Core | 2 |
| `core.copy` | `/copy [n\|btw]` | all | Copy the nth response / the btw answer | Core | 1 |
| `core.export` | `/export` | CC CX (agy U) | Export the conversation to markdown | Core | 2 |
| `core.add-dir` | `/add-dir <path>` | agy CC | Add a workspace root | Core | 2 |
| `core.cd` / `core.pwd` | `/cd`, `/pwd`, `/cwd` | CC CX | Change / print cwd | Core | 2 |
| `core.open` | `/open <path>` | agy | Open a file with `$EDITOR` | Core | 2 |
| `core.mention` | `/mention`, `@file` | CX agy | Attach a file to the prompt | Core | 1 |
| `core.codesearch` | `/codesearch`, `/cs`, `/search` | agy | Fullscreen search (regex, `-F`, `f:`/`path:` glob, `-` exclude) — uses an internal grep library | Core | 2 |
| `core.mcp` | `/mcp [reconnect\|enable\|disable\|verbose]` | all | MCP manager | Core | 3 |
| `core.agents` | `/agents`, `/subagents`, `/list-agents` | all | Pick a custom agent, monitor/kill subagents | Core | 4 |
| `core.tasks` | `/tasks`, `/ps`, `/bashes` | all | Background task / shell log | Core | 4 |
| `core.stop` | `/stop`, `/clean` | CC CX | Stop a background task | Core | 4 |
| `core.background` | `/background`, Ctrl+B | CC agy | Push the current task to the background | Core | 4 |
| `core.hooks` | `/hooks` | all | View active hooks | Core | 6 |
| `core.skills` | `/skills [reload]`, `/reload-skills` | all | Browse/reload skills | Core | 6 |
| `core.plugins` | `/plugin`, `/plugins`, `/reload-plugins` | all | Manage plugins (bundles of skills/agents/hooks/MCP/rules — not native code) | Core | 6+ |
| `core.import` | `/import [codex\|claude\|agy\|gemini]` | CC CX | Import config from another CLI — see `docs/import.md` | Core | 3 |
| `core.schedule` | `/schedule`, `/loop` | agy CC | Timer / cron **in-process** (not cloud) | Compatible recipe — different from CC's `/schedule` (cloud routines) | 6 |
| `core.config` | `/config`, `/settings`, `/debug-config` | all | Settings overlay; `debug-config` = layer + origin | Core | 1 |
| `core.statusline` | `/statusline` | all | Status line script (JSON stdin, format compatible with agy/CC) | Core | 6 |
| `core.title` | `/title [on\|off]` | agy CX | Update the terminal title | Core | 6 |
| `core.keybindings` | `/keybindings`, `/keymap` | all | Editor keybinding | Core | 6 |
| `core.theme` | `/theme`, `/color` | CC CX | Theme | Core | 6 |
| `core.vim` | `/vim` | CX | Vim editing mode | Core | 6 |
| `core.login` / `core.logout` | `/login`, `/logout` | all | Auth via `AuthBroker` | Core + Native OAuth | 1 |
| `core.experimental` | `/experimental` | CX | Toggle feature flags (transport experimental only in global config) | Core | 2 |
| `core.doctor` | `/doctor` | CC | Diagnose install, keyring, config, MCP | Core | 2 |

## 3. FeaturePack commands

### 3.1 `agy`

| id | Alias | Upstream behavior | Mode | Conf | Phase |
|----|-------|------------------|------|------|-------|
| `agy.boost` | `/boost <task>` | Orchestrator plans verifiable subtasks → isolated subagents run in parallel (implement / investigate / verify) → merge → run the full test suite → repeat on failure. Upstream: paid plan. | Compatible recipe (needs multi-agent + worktree) | M | 4–5 |
| `agy.teamwork` | `/teamwork-preview`, `/teamwork` (U) | Phase 1: a scoping interview → a prompt artifact for approval. Phase 2: team roles Sentinel, Project Orchestrator, Explorers (read-only), Workers, Critic, Challenger, Auditor, Success Auditor; request/plan/progress artifacts; exclusive file ownership, a per-agent scratch dir; integrity mode `development\|demo\|benchmark`. Upstream: paid plan. | Compatible recipe (heavyweight) | M | 5+ |
| `agy.grill-me` | `/grill-me <prompt>` | Interviews the user (architecture, error handling, perf, compatibility) before coding; stackable: `/plan /grill-me …` | Compatible recipe | H | 2 |
| `agy.browser` | `/browser <url\|task>` | Browser subagent sandbox (dedicated Chrome profile, allow/deny URL) | Compatible — needs a browser tool (CDP) | M | 6+ |
| `agy.credits` | `/credits` | AI/G1 credits balance + purchase link | Unsupported (purchasing); Native balance if an endpoint is found (U) | L | — |
| `agy.remote-control` | `/remote-control` | Reverse tunnel to the antigravity.google dashboard | Unsupported | H | — |
| `agy.voice` | `/voice`, `/record`, F5 | Dictation (separate OAuth scope, consumer only) | Unsupported | M | — |
| `agy.feedback` | `/feedback` | Send feedback to Google | Unsupported → points to xlightcli's issue tracker | H | — |
| `agy.changelog` | `/changelog` | Release notes | Core-ish: shows xlightcli's changelog | H | 2 |
| `agy.planning` | `/planning` | Legacy (superseded by `/plan`) | Alias → `core.plan` | H | 2 |

### 3.2 `claude`

| id | Alias | Upstream behavior | Mode | Conf | Phase |
|----|-------|------------------|------|------|-------|
| `claude.insights` | `/insights` | Two-tier pipeline: (1) deterministic per-session metrics (tool counts, language, lines ±, time, tool errors…), (2) a model extracts per-session "facets" (goal, outcome, friction, satisfaction…), then a model writes a narrative report → a self-contained **HTML** file. Upstream reads up to 200 unanalyzed sessions, skips sessions that are too short, caches per session. | Compatible — the data source is **xlightcli's own** sessions (SQLite); output `$DATA/insights/<workspace>/insights-<ts>.html`, returns the path (D-021) | H (pipeline), U (layout) | 2 |
| `claude.team-onboarding` | `/team-onboarding` | Onboarding guide markdown generated from 30 days of usage | Compatible (share link Unsupported) | H | 6 |
| `claude.simplify` · `claude.batch` · `claude.debug` · `claude.fewer-permission-prompts` · `claude.verify` · … | names unchanged | Bundled skills/prompt packs from Claude Code | Compatible — shipped as a skill bundle (proprietary content not copied; rewritten) | M | 6 |
| `claude.advisor` | `/advisor` | Second-model advisor | Compatible recipe (secondary model) | M | 6 |
| `claude.upgrade` · `/rate-limit-options` · `/usage-credits` · `/privacy-settings` · `/passes` | — | claude.ai billing/plan page | Unsupported (opens URL) | H | — |
| `claude.cloud` | `/remote-control`, `/remote-env`, `/teleport` (`/tp`), `/web-setup`, `/autofix-pr`, `/ultrareview`, `/desktop`, `/mobile`, `/routines` | Cloud session / app | Unsupported | H | — |
| `claude.integrations` | `/chrome`, `/install-github-app`, `/install-slack-app`, `/design*`, `/artifacts`, `/voice`, `/feedback`, `/bug`, `/release-notes`, … | Product integrations | Unsupported (except `/ide` → Core later) | M | — |

### 3.3 `codex`

| id | Alias | Upstream behavior | Mode | Conf | Phase |
|----|-------|------------------|------|------|-------|
| `codex.personality` | `/personality` | Response style (only shown when the catalog enables it) | Compatible (system prompt preset) | H | 6 |
| `codex.archive` / `codex.delete` | `/archive`, `/delete` | Archive / delete session | Core-ish (maps to the session store) | H | 2 |
| `codex.raw` | `/raw` | Raw scrollback | Core-ish | H | 6 |
| `codex.worktree` | `/worktree` | Work inside a worktree | Map → `WorkspaceManager` | M | 5 |
| `codex.apps` | `/apps` | ChatGPT connectors | Unsupported | H | — |
| `codex.cloud` | `/cloud`, `/cloud-environment`, `/app` | Codex cloud / Desktop app | Unsupported | H | — |
| `codex.feedback` | `/feedback` | Send logs to OpenAI | Unsupported → issue tracker | H | — |
| — | `/setup-default-sandbox`, `/sandbox-add-read-dir` | Windows-only | N/A (D-003) | H | — |
| — | `/rollout`, `/test-approval`, `/debug-m-*` | Debug build | Skipped | H | — |

## 4. Execution modes & permission presets

| xlightcli | agy | Claude Code | Codex |
|-----------|-----|-------------|-------|
| mode `default` (permission `ask`) | `default` + `toolPermission=request-review` | `default` | approval `on-request` |
| mode `accept-edits` (`auto-edit`) | `accept-edits` | `acceptEdits` | `auto` edit within the workspace |
| mode `plan` (read-only tools + plan artifact) | `plan` | `plan` | `/plan` |
| permission `strict` (asks for every non-read tool) | `toolPermission=strict` | rule ask | `untrusted` |
| permission `full-auto` | `always-proceed`, `--dangerously-skip-permissions` | `bypassPermissions` | `never` + full access |
| `sandboxed-auto` (Phase 6+, requires OS sandbox) | `proceed-in-sandbox` | `/sandbox` | sandbox `workspace-write` |

Internal rule format: `action(target)` with `*` and `regex:`; precedence order **deny > ask > allow** (matches agy/CC).
Action: `read_file`, `write_file`, `command`, `read_url`, `mcp`, `unsandboxed`.

## 5. Headless / print mode compatibility

`xlightcli exec` accepts the familiar syntax as well (D-026):

| Flag | Compatible with |
|------|-------------|
| `-p, --print "<prompt>"` | agy, CC |
| `--output-format text\|json\|stream-json` | agy, CC |
| `--input-format`, `--json-schema` | agy |
| `--model`, `--effort`, `--mode`, `--agent` | agy, CC, CX |
| `-c, --continue`, `--conversation <id>` / `--resume <id>` | agy, CC |
| `--add-dir`, `--print-timeout` (default 5m) | agy |
| `--dangerously-skip-permissions` | agy, CC (requires workspace trust + global opt-in) |
| Exit code `0` ok · `1` general error · `2` bad input · `3` model/agent error after output was already produced | agy |

JSON output: `{conversation_id, status, response, usage{input,output,thinking,cache_read,total}_tokens}`.
In print mode, read-only commands (`/usage`, `/model`, `/effort`, `/skills`, `/permissions`, `/hooks`, `/help`, `/config`)
print TSV/JSON without running an agent turn; interactive commands return exit error `2`.

## 6. Native data sources for `/usage` (unverified — Phase 0 spike)

| Transport | Source | Conf |
|-----------|-------|------|
| `chatgpt` | Header `x-codex-primary-*`, `x-codex-secondary-*`, `x-codex-credits-*`; SSE event `codex.rate_limits`; `GET {base}/wham/usage` | H (source Codex) |
| `claude-subscription` | Header `anthropic-ratelimit-unified-*`; endpoint usage OAuth (scope `user:profile`) | U |
| `anthropic-api` | Standard API header `anthropic-ratelimit-*` | M |
| `antigravity` | `v1internal:retrieveUserQuotaSummary` (buckets 5h/weekly, `remainingFraction`, `resetTime`); fallback `fetchAvailableModels` `quotaInfo` | U (from OpenCodex) |
| `gemini-api` | No equivalent public quota endpoint; only per-response usage | M |
