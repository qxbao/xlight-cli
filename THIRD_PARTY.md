# Third-party code

xlightcli is licensed under GPL-3.0-only (see `LICENSE`). This file records source code ported or adapted
from other projects, as required by their licenses. Rust crate dependencies are tracked by `cargo deny`,
not here.

## OpenCodex

- Repository: <https://github.com/lidge-jun/opencodex>
- License: MIT
- Reviewed at commit: `3cc34e1181926b64331490fdcfee162ffb62fe73` (2026-09-28)
- Usage: selective port from TypeScript to Rust (see `docs/PLAN.md` §4.5)

```text
MIT License

Copyright (c) 2026 opencodex contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### Ported files

| xlightcli file | Upstream file | Upstream commit | Date |
|----------------|---------------|-----------------|------|
| `crates/provider-codex/src/consts.rs` | `src/oauth/chatgpt.ts, src/oauth/chatgpt-device.ts` | `3cc34e1` | 2026-09-28 — OAuth client id, endpoints, scope, device-grant endpoints |
| `crates/provider-codex/src/auth.rs` | `src/oauth/chatgpt.ts (extractAccountId), src/oauth/chatgpt-device.ts` | `3cc34e1` | 2026-09-28 — JWT account-id fallback chain; two-step device grant |
| `crates/provider-claude/src/consts.rs` | `src/oauth/anthropic.ts, src/adapters/client-fingerprint.ts` | `3cc34e1` | 2026-09-28 — OAuth constants; client fingerprint headers |
| `crates/provider-claude/src/auth.rs` | `src/oauth/anthropic.ts, src/oauth/local-token-detect.ts` | `3cc34e1` | 2026-09-28 — OAuth flow; credential discovery |
| `crates/provider-claude/src/transport_subscription.rs` | `src/adapters/client-fingerprint.ts` | `3cc34e1` | 2026-09-28 — Subscription request headers |
| `crates/provider-agy/src/consts.rs` | `src/oauth/google-antigravity.ts, src/adapters/client-fingerprint.ts` | `3cc34e1` | 2026-09-28 — OAuth/endpoint constants, User-Agent |
| `crates/provider-agy/src/auth.rs` | `src/oauth/google-antigravity.ts, src/oauth/account-import/google-antigravity-adapter.ts, src/adapters/client-fingerprint.ts` | `3cc34e1` | 2026-09-28 — Google OAuth, loadCodeAssist/onboardUser, import |
| `crates/provider-agy/src/wire/request.rs` | `src/adapters/google.ts, src/adapters/google-antigravity-wire.ts, src/adapters/google-tool-schema.ts` | `3cc34e1` | 2026-09-28 — Gemini/CCA request envelope, tool schema sanitizing |
| `crates/provider-agy/src/wire/response.rs` | `src/adapters/google.ts (parseStream)` | `3cc34e1` | 2026-09-28 — Stream chunk parsing |
| `crates/provider-agy/src/transport_antigravity.rs` | `src/adapters/google.ts` | `3cc34e1` | 2026-09-28 — Cloud Code Assist transport |
| `crates/provider-agy/src/quota.rs` | `src/providers/quota/antigravity.ts` | `3cc34e1` | 2026-09-28 — Quota summary parsing |

## openai/codex (reference only)

- Repository: <https://github.com/openai/codex>
- License: Apache-2.0 (compatible with GPL-3.0, D-028)
- Read at commit `1cc7e2361237ce7244430ee1d581c77f95c57ac8` to verify the `/responses` request shape and
  rate-limit headers. **No code copied.** If code is ported later, add the Apache-2.0 notice, keep upstream
  NOTICE content, and state changes per Apache-2.0 §4.
