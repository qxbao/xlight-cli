# CLAUDE.md

@AGENTS.md

## Notes specific to Claude Code

- `AGENTS.md` (imported above) is the source of truth for the rules; this file only adds the parts specific to Claude Code. Don't duplicate rules here — edit `AGENTS.md` instead.
- For tasks touching multiple crates or a public trait: use plan mode, present the design and relevant invariants (`docs/PLAN.md` §2) before changing code.
- Quick lookup:
  - What a crate does, what it's allowed to depend on: `CODEBASE.md` §2–§3.
  - Code pattern for the task at hand: `PATTERNS.md` (table of contents at the top of the file).
  - Current phase and exit criteria: `docs/PLAN.md` §19.
- Library docs (tokio, ratatui, reqwest, rusqlite, rmcp, …): look up current docs instead of relying on memory; ratatui's and rmcp's APIs change quickly.
- Don't run commands that hit the network to a real provider yourself (live test, `dev probe`) — suggest the user run it with `! <command>`.
- Reply to the user in Vietnamese; everything written to the repo is English (D-015, D-029).
