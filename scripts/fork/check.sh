#!/usr/bin/env bash
# The gate every fork change and upstream sync must pass before landing: upstream's own
# frontend and Rust checks, minus its Windows and Linux legs. RB_LITE_TEST makes the Rust
# tests refuse writes to an installed rekordbox library.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
# Upstream CI uses Node 24; the Mini's default node is older.
[[ -d /opt/homebrew/opt/node@24/bin ]] && PATH="/opt/homebrew/opt/node@24/bin:$PATH"
PATH="$HOME/.cargo/bin:$PATH"
pnpm install --frozen-lockfile
pnpm lint
pnpm typecheck
pnpm test
RB_LITE_TEST=1 cargo test --workspace
