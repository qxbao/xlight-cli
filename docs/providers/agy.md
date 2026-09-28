# Provider `agy` — Google Antigravity / Gemini

> Updated **2026-09-28** (Wave 2/3). Labels: **H** docs/source · **M** inference · **U** unverified · **verified <date>** after a spike.
> Nothing goes into `consts.rs` without a verified label — every `antigravity` constant below is
> still **U** (ported from OpenCodex, not tested against a real account).

## Transports

| Transport | Stability | Auth | Notes |
|-----------|-----------|------|-------|
| `gemini-api` | Stable (**H**, official Google docs) | `GEMINI_API_KEY` (+ `GOOGLE_GEMINI_BASE_URL`) | Gemini API `streamGenerateContent`, header `x-goog-api-key` |
| `antigravity` | **Experimental** (D-002) | Google OAuth (PKCE) | Cloud Code Assist backend |

## `gemini-api` (H)

- Default base URL `https://generativelanguage.googleapis.com`, overridable via `GOOGLE_GEMINI_BASE_URL`.
- `POST {base}/v1beta/models/{model}:streamGenerateContent?alt=sse`, header `x-goog-api-key`.
- `list_models`: `GET {base}/v1beta/models`.
- `quota()` → `Ok(None)` (the public API has no per-key quota endpoint).
- Response shape (unwrapped): `{candidates:[{content:{parts:[...]}, finishReason}], usageMetadata, promptFeedback}`.

## `antigravity` — ported from OpenCodex (U, not live-verified)

Source: OpenCodex (MIT) `@ 3cc34e1181926b64331490fdcfee162ffb62fe73` —
`src/oauth/google-antigravity.ts`, `src/adapters/google-antigravity-wire.ts`, `src/adapters/google.ts`,
`src/providers/antigravity-models.ts`, `src/providers/quota/antigravity.ts`,
`src/oauth/account-import/google-antigravity-adapter.ts`, `src/adapters/client-fingerprint.ts`.
Everything below was read directly from the file contents at that commit (not guessed from file names).

- **Stream**: `POST {cca_base}/v1internal:streamGenerateContent?alt=sse`, `cca_base` defaults to
  `https://cloudcode-pa.googleapis.com`.
- **Envelope**: `{model, userAgent:"antigravity", requestType:"agent", project, requestId:"agent-<uuid>",
  request:{...generateContent body, sessionId}}`. `sessionId` lives **inside** `request` (camelCase),
  with no top-level/snake_case duplicate. `userAgent:"antigravity"` is a fixed body constant,
  **distinct** from the real HTTP `User-Agent` header (see below).
- **HTTP `User-Agent`**: `antigravity/ide/{ver} (os_type=windows; arch=amd64; aidev_client; auth_method=oauth)`,
  `ver` defaults to `2.5.5` (decompiled from the Antigravity IDE's Go language server per the
  ported source's comment), overridable via `GOOGLE_ANTIGRAVITY_USER_AGENT`. ⚠️ This is **client
  impersonation** (R-10): the backend answers 404 for newer models (`gemini-3.7-*` and up) if the
  UA isn't shaped like `antigravity/ide/...` — there is no "honest" alternative way to reach those
  models through this transport. This is exactly why it's experimental and off by default (D-002).
- **Response shape**: every SSE data frame is `{"response": {candidates:[...], usageMetadata, promptFeedback}}`
  — the plain Gemini payload **wrapped** in a `response` field. Inline errors:
  `{"error": {code, message, status}}` (no wrapper).
- **OAuth**: PKCE; `client_id`/`client_secret` are the public "Desktop app" OAuth client identifiers
  (not a user secret) — `<antigravity-oauth-client-id: not committed>` /
  `<antigravity-oauth-client-secret: not committed>`. Endpoints: `accounts.google.com/o/oauth2/v2/auth`,
  `oauth2.googleapis.com/token` (+ `/revoke`), `www.googleapis.com/oauth2/v2/userinfo`. Scopes:
  `cloud-platform`, `userinfo.email`, `userinfo.profile`, `cclog`, `experimentsandconfigs`. Fixed
  loopback callback `127.0.0.1:51121/callback` (a port pre-registered with Google, not ephemeral).
  After login: `POST {cca_prod}/v1internal:loadCodeAssist {metadata:{ideType:"ANTIGRAVITY"}}`; if no
  project yet → `POST {cca_daily}/v1internal:onboardUser {tier_id:"free-tier", metadata:{ide_type,
  ide_name:"antigravity", ide_version}}` (the ported source polls up to 5 times with 429/5xx
  backoff; xlightcli's Phase 0 implementation only tries once — see "Gaps" below). The discovered
  project id is now persisted on `xlightcli_auth::AccountInfo.metadata["antigravity_project_id"]`
  by `AgyAuthAdapter::login`/`refresh` (Wave 3; see gap #1's resolution below).
- **Model catalog**: `POST {cca_daily}/v1internal:fetchAvailableModels {project}` → `{models: {<wireId>:
  {displayName, quotaInfo|quotaInfos, maxTokens, maxOutputTokens, ...}}}`. The real catalog
  (OpenCodex) has complex logic (picker collapsing, retired-tier aliasing, suffix effort encoding)
  — **not ported** in Phase 0, only the flat fields are read.
- **Effort** (`/effort`): mapped directly to `generationConfig.thinkingConfig.thinkingLevel`
  (`low`/`medium`/`high`) for both transports — **simplified** vs. the source
  (`resolveAntigravityEffortWireModel` chooses between a model-id suffix swap or `thinkingLevel`
  depending on the model).
- **sessionId**: `sha256(firstUserText)` → big-endian `u64` masked with `0x7FFF_FFFF_FFFF_FFFF`,
  prefixed with `-` (`CLIProxyAPI generateStableSessionID`); falls back to a random id when there is
  no user text yet. Must stay stable across turns of the same conversation (used to replay
  `thoughtSignature`) — xlightcli does not port the source's more elaborate "Codex thread anchor"
  logic (no equivalent concept in the current protocol).
- **Quota** (`/usage`): `POST {cca_daily}/v1internal:retrieveUserQuotaSummary {project}` →
  `groups[].buckets[]` (`window`/`bucketId` containing "5h"/"week", `remainingFraction`/
  `remainingPercentage`, `resetTime`); falls back to `fetchAvailableModels[].quotaInfo`.
  `xlightcli_protocol::QuotaSnapshot` is flat (a single `used_percent`, not multiple windows) —
  xlightcli picks the Gemini 5h window as primary, keeping the full parsed JSON in `detail`.
- **Per-tool-call `thoughtSignature`**: Gemini attaches a signature directly to the `functionCall`
  part that produced it (confirmed from the ported source's `googleToolCallMetadataFromPart`).
  Since Wave 3, this rides `ContentBlock::ToolUse.opaque` (protocol addition, see gap #3's
  resolution below) and is replayed on the outbound `functionCall` part only for a matching
  `(provider, transport)` — `wire::request::matching_thought_signature`,
  `wire::response::Translator::handle_part`. Not ported: the source's separate
  replay-cache/`applyAntigravityReplay` mechanism for re-signing across turns when no signature was
  observed; xlightcli only ever replays a signature it actually saw.

## Known gaps (Phase 0/3 — flagged to the team lead / auth crate owner)

1. ~~No place to store the Cloud Code Assist `project_id` on the credential~~ **Resolved (Wave 3)**:
   `xlightcli_auth::AccountInfo` gained a non-secret `metadata: serde_json::Value` field (persisted
   by `AccountIndex`/`AccountRecord.metadata`, already existed there). `AgyAuthAdapter::login`/
   `refresh` store the discovered project id under `metadata["antigravity_project_id"]`;
   `transport_antigravity::resolve_project_id` reads it from `CredentialHandle::account()`, with
   `AgyEndpoints::antigravity_project_id` (env `GOOGLE_ANTIGRAVITY_PROJECT_ID`) kept only as an
   explicit override. **Residual gap**: `AuthBroker::run_refresh` (`crates/auth/src/handle.rs`)
   currently only takes `refreshed.secret` from what `AuthAdapter::refresh` returns, not
   `refreshed.account` — so a project id (re-)discovered during a refresh is not yet propagated
   into the live `AccountEntry`/persisted `AccountIndex` row. Only `login()`'s initial persistence
   path (`AuthBroker::persist_and_cache`) picks up metadata today. A broker-side fix (have
   `run_refresh` also persist `refreshed.account`) is out of scope for this change.
2. ~~`exchange_code_for_token`/`refresh_access_token` take no `client_secret`~~ **Resolved (Wave 3)**:
   additive `exchange_code_for_token_with_secret`/`refresh_access_token_with_secret` (`client_secret:
   Option<&SecretString>`) added to `xlightcli_auth::oauth`; `AgyAuthAdapter` now uses them with
   `consts::antigravity::OAUTH_CLIENT_SECRET`. Still **U**: whether Google's token endpoint actually
   requires it for this client (not live-verified).
3. ~~Per-functionCall `thoughtSignature` not carried~~ **Resolved (Wave 3)**: see the
   `ContentBlock::ToolUse.opaque` protocol addition above (`docs/CONTRACTS.md` §1).
4. **`onboardProject` only tries once** (the source polls up to 5 times with 429/5xx backoff) — a
   brand-new account may need a few seconds for onboarding to finish; not polled yet.
5. **Model catalog / effort ladder are simplified** — see above; no alias collapsing, no
   retired-tier handling.

## Before enabling `antigravity-subscription` for real users

- [ ] Verify the endpoint + envelope + UA against a real account (manual live test,
      `XLIGHTCLI_LIVE_AGY=1 cargo test ... -- --ignored live_` — no `live_` test exists yet in
      Phase 0; add one once a test account is available).
- [ ] Determine where agy stores the real OAuth token (keyring/file) for `discover_existing()` —
      currently returns empty since unverified (only
      `oauth/account-import/google-antigravity-adapter.ts` proves importing from an external
      "Cockpit" record, not from a local file).
- [ ] Verify `retrieveUserQuotaSummary` and the plan tier (`loadCodeAssist`) return the shape
      that's been ported.
- [ ] Decide on the residual broker-side refresh-metadata-propagation gap (#1 above).
- [ ] Confirm whether client_secret is actually required by Google's token endpoint (#2 above).
- [ ] Check whether gap #4 (single-attempt onboarding) actually causes failures for brand-new
      accounts.
