# Provider `claude` — Anthropic

> Research notes **2026-09-28** (Wave 2). Labels: **H** · **M** · **U** · **verified <date>**.
> Port source: OpenCodex (MIT) `github.com/lidge-jun/opencodex` @ `3cc34e1181926b64331490fdcfee162ffb62fe73`,
> `src/oauth/anthropic.ts`, `src/oauth/local-token-detect.ts`, `src/adapters/anthropic.ts`,
> `src/adapters/client-fingerprint.ts` (docs/PLAN.md §4.5). OpenCodex is a third-party proxy, **not**
> Anthropic's official documentation — every **M** label below means "observed from real running code",
> not yet live-verified by us.

## Transports

| Transport | Stability | Auth | Notes |
|-----------|-----------|------|---------|
| `anthropic-api` | Stable | `ANTHROPIC_API_KEY` / `x-api-key` | Messages API SSE (public, documented, **H**) |
| `claude-subscription` | **Experimental** (D-002), feature `claude-subscription` | Claude OAuth (Pro/Max) | Messages API + OAuth bearer + `anthropic-beta` header + client-fingerprint headers (**M**) |

Both transports share the same wire translator (`wire::response::Translator`) and request builder
(`wire::request::build_body`); they differ via `RequestOptions::oauth_mode` (system instruction +
tool-name prefix) and headers (`transport_api.rs` vs `transport_subscription.rs`).

## `anthropic-api` — endpoint & wire (implemented, Phase 0)

- Base URL: `https://api.anthropic.com`; `POST /v1/messages`; `GET /v1/models`. **H**, public docs
  (docs.anthropic.com / platform.claude.com), 2026-09-28.
- Header: `x-api-key: <key>`, `anthropic-version: 2023-06-01`, `content-type: application/json`,
  `accept: text/event-stream` (streaming). **H**.
- SSE event set implemented in `wire/response.rs`: `message_start`, `content_block_start`
  (`text`/`thinking`/`redacted_thinking`/`tool_use`), `content_block_delta`
  (`text_delta`/`thinking_delta`/`signature_delta`/`input_json_delta`), `content_block_stop`,
  `message_delta` (stop_reason, cumulative usage), `message_stop`, `ping`, `error`. **H**, public
  docs + cross-checked against OpenCodex's `parseStream`.
- `stop_reason` mapping: `end_turn`→`EndTurn`, `tool_use`→`ToolUse`, `max_tokens`→`MaxTokens`,
  `refusal`/`content_filter`→`Refusal`, everything else (`pause_turn`, `stop_sequence`, …) preserved
  verbatim via `StopReason::Other` rather than guessed.
- Rate-limit headers: `anthropic-ratelimit-requests-{limit,remaining,reset}`. **M** — docs describe
  the `anthropic-ratelimit-*` family; the exact bucket name ("requests") is the most consistently
  documented one as of this date, but there are other buckets (input/output tokens) we don't parse
  yet. `quota()` for this transport always returns `Ok(None)` (no plan/usage endpoint for a bare API
  key).
- `list_models`: `GET /v1/models` → `{"data": [{"id", "display_name", ...}]}`. **H** shape, but
  `context_window`/`max_output_tokens` aren't in that response, so `ModelInfo` leaves them `None`
  rather than guessing.
- Extended thinking: `thinking: {"type": "enabled", "budget_tokens": N}`, mapped from
  `ReasoningEffort` via a simple 4-tier table in `wire/request.rs` (Minimal→1024, Low→4096,
  Medium→8192, High→16384). **M** — a documented, chosen mapping, not Anthropic's own scale.
  **Known gap (not implemented):** newer model families (per OpenCodex comments: Sonnet ≥5, Opus
  ≥4.7, and a "fable" family) have moved to an *adaptive* thinking wire
  (`thinking: {"type": "adaptive", ...}` + `output_config.effort`) and reject the classic
  `"enabled"` form outright, while older families reject `"adaptive"`. This adapter only implements
  the classic `"enabled"` form — a request to a newer-family model with `reasoning` set will 400
  upstream. Flagged for the live spike / a follow-up change.

## `claude-subscription` — OAuth (experimental, feature-gated)

All of the following is **M** (OpenCodex-sourced, not live-verified by us) unless noted:

- OAuth authorize endpoint: `https://claude.ai/oauth/authorize` (note: `claude.ai`, not
  `api.anthropic.com`).
- Token endpoint: `https://api.anthropic.com/v1/oauth/token`. Body is **JSON** (not form-encoded):
  `{"grant_type": "authorization_code", "client_id", "code", "state", "redirect_uri",
  "code_verifier"}` for the exchange, `{"grant_type": "refresh_token", "client_id",
  "refresh_token"}` for refresh. Response: `{"access_token", "refresh_token", "expires_in",
  "account": {"uuid", "email_address"}}`.
- Public client id `9d1c250a-e61b-44d9-88ed-5944d1962f5e` (base64-decoded from OpenCodex source).
- Scopes: `org:create_api_key user:profile user:inference`. Fixed loopback callback port `54545`,
  path `/callback`.
- Every OAuth-authenticated Messages request must carry `anthropic-beta:
  claude-code-20250219,oauth-2025-04-20` and a first `system` block reading exactly
  *"You are a Claude agent, built on Anthropic's Claude Agent SDK."* — omitting either reportedly
  gets the request rejected as non-first-party.
- Client-fingerprint headers (`X-App: cli`, `X-Stainless-*` matching `@anthropic-ai/sdk 0.74.0` /
  Claude Code 2.1.63, `X-Claude-Code-Session-Id`, `x-client-request-id`) are sent alongside the
  bearer token — per OpenCodex's own comment, an OAuth token with an otherwise-empty header set is
  itself a "non-first-party" signature upstream can flag. Implemented in
  `transport_subscription.rs`/`consts.rs`; session id is a random per-process UUID rather than
  OpenCodex's deterministic token-hash derivation (simplification, **M**, documented deviation).
- Tool names: OAuth tokens reportedly reject "unprefixed" custom tool names; every non-builtin tool
  name gets a `custom_` prefix on the way out (and is stripped on the way back for `tool_use`, not
  yet implemented for the return path — see "Known gaps" below).
- Credential file: `<config-dir>/.credentials.json` where config-dir is `$CLAUDE_CONFIG_DIR` or
  `~/.claude`; payload `{"claudeAiOauth": {"accessToken", "refreshToken", "expiresAt"}}`
  (epoch-ms). **H** on Linux (0600 file). macOS additionally checks Keychain service
  `"Claude Code-credentials"` **first** — **not implemented** here (no keyring dependency in this
  crate; `auth::store` territory, Phase 2), so on macOS this adapter only finds the file fallback,
  same as Claude Code itself when the Keychain is unavailable.
- Quota: `anthropic-ratelimit-unified-{5h,7d}-{utilization,reset}` response headers on subscription
  calls. **U** — community-sourced only (no OpenCodex confirmation seen), not cross-checked against
  a real subscription account. `quota()` caches whatever it last saw from a `stream()` response and
  returns that (`Ok(None)` until the first successful call).
- `list_models()` for this transport is a deliberate empty-list placeholder (not a network call):
  whether `GET /v1/models` even accepts an OAuth bearer is unverified, and AGENTS.md §6 forbids
  hardcoding guessed model ids without a live-verified spike.

### Known gaps / deviations (Phase 0 scope cuts)

- Adaptive-thinking wire (see above) not implemented — only affects `reasoning` + newer models.
- `import.rs` only reads project `.mcp.json` and the top-level `mcpServers` key of
  `~/.claude.json` — not the nested `projects["<abs path>"].mcpServers` section (docs/import.md
  marks that variant **M**). Left for Phase 3's richer import.
- `ConfigImporter`/discovery wired in Phase 0 rather than Phase 3 as CODEBASE.md's original table
  suggested, at the team lead's explicit request for this wave — see final report for the
  contract-note.

## `/insights` (Compatible, D-021)

Upstream pipeline (H for the pipeline, U for detailed layout):
1. Deterministic per-session metrics (message/tool counts, language, lines ±, files, response time, tool errors, MCP/web/subagent usage…).
2. A model extracts per-session "facets" (goal, category, outcome, friction, satisfaction, summary) — cached per session.
3. A model writes a narrative report → a self-contained HTML file: stats, "At a Glance", What You Work On, How You Use…, Where Things Go Wrong, Features to Try (suggests additions to the rules file), charts (tools, languages, outcomes, friction…).

xlightcli: the source is its own sessions in SQLite; up to 200 unanalyzed sessions per run; facets cached in a dedicated table; output `$DATA/insights/<workspace>/insights-<ts>.html`, returns the path.

**Phase 0 status:** `features::insights::execute` always returns `CommandResult::Unavailable` —
there is no session storage/facet cache yet (`xlightcli-storage` is still an empty skeleton). See
`src/features/insights.rs` for the full module doc on the planned pipeline.

## Live verification checklist (before enabling `claude-subscription` for real users)

- [ ] `anthropic-api` streaming + tool use + thinking signature — confirm via `dev probe` with a
      real API key (not run automatically in CI/agent — only run manually by a user, AGENTS.md §7).
- [ ] Confirm the OAuth authorize/token endpoint, client id, and scopes are still correct (Anthropic may rotate the
      client id periodically).
- [ ] Confirm the `anthropic-beta` value and client-fingerprint headers are still required / still on the correct
      version (`@anthropic-ai/sdk` version drifts over time).
- [ ] Confirm the `anthropic-ratelimit-unified-*` header really appears on subscription-account
      responses (currently **U**, no independent evidence beyond OpenCodex).
- [ ] Confirm which model families need the "adaptive thinking" wire instead of `"enabled"` (see "Known
      gaps" above) before enabling `reasoning` for `claude-subscription` in production.
- [ ] macOS discovery: confirm whether reading the Keychain (`"Claude Code-credentials"`) is required, or whether the file fallback
      is sufficient in practice.

## Live verification log

| Date | Check | Result |
|---|---|---|
| 2026-09-28 | Browser OAuth token exchange (form-encoded) | **400 Invalid request format** → switched to JSON body + `code#state` split (OpenCodex `exchangeToken`) |
| 2026-09-28 | `dev probe claude --transport claude-subscription --model claude-sonnet-5` with an imported Claude Code credential | **OK**: text streamed, usage parsed, `stop=EndTurn` |
