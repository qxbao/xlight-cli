# Config import — Claude Code / Codex / agy → xlightcli

> Research date **2026-09-28**. Confidence: **H** docs/source · **M** inferred · **U** unverified.
> `/import` and `xlightcli init` use this table. Import **reads** (read-only) the other CLI's files, normalizes them
> to the xlightcli format, **does not** write back, and **does not** call the binary (INV-1). Credentials follow D-017 (import & own).
>
> Each provider crate implements `ConfigImporter` (PLAN §4.1) — the vendor format lives in the adapter; the runtime only receives
> canonical fragments: `McpServerConfig`, `RuleFile`, `SkillRef`, `HookConfig`, `AgentProfile`, `PermissionRule`, `Keybinding`.

## 1. Mapping table

| xlightcli concept | Claude Code | Codex | agy |
|---------------------|-------------|-------|-----|
| **Credential** | `~/.claude/.credentials.json` (0600, Linux); macOS Keychain item "Claude Code-credentials" (U); env `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN` | `~/.codex/auth.json` (`auth_mode`, `OPENAI_API_KEY`, `tokens{id_token,access_token,refresh_token,account_id}`, `last_refresh`); or keyring service "Codex Auth" per `cli_auth_credentials_store` | OAuth token in keyring or file (location U); env `GEMINI_API_KEY` (+ `GOOGLE_GEMINI_BASE_URL`) |
| **Global settings** | `~/.claude/settings.json` (+ managed `/etc/claude-code/managed-settings.json`); `CLAUDE_CONFIG_DIR` | `~/.codex/config.toml` (`$CODEX_HOME`); profile = `~/.codex/<name>.config.toml` (as of 0.134) | `~/.gemini/antigravity-cli/settings.json` |
| **Project settings** | `.claude/settings.json`, `.claude/settings.local.json` | `.codex/config.toml` (ignores the provider/auth/profile keys) | `.agents/…` |
| **Rules / instructions** | `CLAUDE.md` (root, `~/.claude/CLAUDE.md`), `rules/*.md` (frontmatter `paths`), also reads `AGENTS.md` | `~/.codex/AGENTS.override.md` › `~/.codex/AGENTS.md`; project: from the repo root down to cwd, each dir `AGENTS.override.md` › `AGENTS.md` › fallback names | `~/.gemini/{AGENTS,GEMINI}.md`, `~/.gemini/config/rules/*.md`; workspace `AGENTS.md`/`GEMINI.md` at every level, `.agents/rules/*.md`; frontmatter `trigger: always_on\|model_decision\|glob\|manual`, `description`, `globs`; inline `@[label](path)`; limit 24 KB/file, 20k tokens total |
| **MCP** | `.mcp.json` `{"mcpServers":{…}}` (`${VAR}` expansion); user: `mcpServers` in `~/.claude.json`; local: `projects["<abs>"].mcpServers` (M) | `[mcp_servers.<id>]` `command,args,env,cwd,url`, bearer token env var, `default_tools_approval_mode` | `~/.gemini/config/mcp_config.json`, `.agents/mcp_config.json`: `{"mcpServers":{n:{command,args,env,cwd \| serverUrl,headers,authProviderType,oauth{clientId,clientSecret},disabled,disabledTools}}}` |
| **Skills** (→ slash command) | `skills/<name>/SKILL.md` (project `.claude/` + home) | `.agents/skills` (cwd → repo root), `~/.agents/skills`, `/etc/codex/skills`; prompts `~/.codex/prompts/*.md` (deprecated) | `.agents/skills/<dir>/SKILL.md` (legacy `.agent/skills`), `~/.gemini/antigravity-cli/skills/`, `~/.gemini/config/skills/`; frontmatter `name`, `description` (required) |
| **Custom agents** | `agents/*.md` (frontmatter) | subagents (config) | `.agents/agents/<name>.md` or `<name>/agent.md`, `~/.gemini/config/agents/`; frontmatter `name, description, tools[], model: inherit\|flash\|pro, commandExecutionPolicy, skills, inheritCustomizations` |
| **Hooks** | `settings.json` `"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"…"}]}]}` | `~/.codex/hooks.json`, `.codex/hooks.json`, or `[hooks]` in config.toml | `.agents/hooks.json`, `~/.gemini/config/hooks.json`: `{"<name>":{"enabled":true,"PreToolUse":[{"matcher":"run_command\|view_file","hooks":[{"type":"command","command":"…","timeout":10}]}]}}`; event PreToolUse/PostToolUse/PreInvocation/PostInvocation/Stop; decision `allow\|deny\|ask\|force_ask\|deny_unless_prior_grant` |
| **Permissions** | `permissions.{allow,ask,deny}` in settings | approval policy + sandbox mode | `permissions:{allow,ask,deny}` with `action(target)`, `*`, `regex:` |
| **Keybindings** | `~/.claude/keybindings.json` | `/keymap` config | `~/.gemini/antigravity-cli/keybindings.json` (action id → keys; `[]` = disable; layout U) |
| **Status line** | `statusLine` in settings | `/statusline` | `statusLine:{type:"command",command,padding,enabled}`; script receives JSON stdin (`cwd, session_id, model{id,display_name}, context_window, quota, execution_mode, …`) |
| **Plugins** | plugin dirs | `/plugins` | `plugin.json` + `mcp_config.json`, `hooks.json`, `skills/`, `agents/`, `rules/`; `.agents/plugins/`, `~/.gemini/config/plugins/` |
| **Transcripts** (for optional `/insights` import) | `~/.claude/projects/<proj>/<session>.jsonl` | `~/.codex/sessions/` (U) | U |

## 2. Import rules

- **Trust:** hooks, MCP stdio, and `command` permission-allow rules imported from project scope only take effect after workspace trust (PLAN §12.3).
- **Don't copy secrets into config:** MCP env/header values containing tokens ⇒ converted into an `${ENV}` reference or stored in the MCP credential store; ask the user.
- **Hook event mapping:** `PreToolUse` / `PostToolUse` / `Stop` keep their names; `PreInvocation` / `PostInvocation` (agy) ↔ `before_turn` / `after_turn`. Tool names in `matcher` are mapped to xlightcli tool names (`Bash`/`run_command` → `shell`, `Read`/`view_file` → `read_file`, …) via an alias table in the importer.
- **Skills / agents / rules** are not copied as files: xlightcli reads the standard paths directly (`.agents/skills`, `.agents/rules`, `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`) per the `rules.sources` / `skills.sources` config — so a repo can be shared across multiple CLIs without needing to sync.
- **Import report:** prints an "imported / skipped (reason) / needs trust" table — nothing is silently skipped.
