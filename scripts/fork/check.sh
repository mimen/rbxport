#!/usr/bin/env bash
# The gate every fork change and upstream sync must pass before landing: upstream's own
# frontend and Rust checks, minus its Windows and Linux legs. RB_LITE_TEST makes the Rust
# tests refuse writes to an installed rekordbox library.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
# Upstream CI uses Node 24; the Mini's default node is older.
[[ -d /opt/homebrew/opt/node@24/bin ]] && PATH="/opt/homebrew/opt/node@24/bin:$PATH"
PATH="$HOME/.cargo/bin:$PATH"
# mp3lame-sys runs autotools, which breaks on a space in its out dir (hub worktrees live under "Application Support").
[[ "$PWD" == *" "* ]] && export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/rbxport-fork-target}"
pnpm install --frozen-lockfile
pnpm lint
pnpm typecheck
pnpm test
# Two wall-clock tests upstream's CI runs but the hub's headless sessions cannot pass: their timers are
# coalesced (a 5 ms sleep takes 30 ms), so the decks underrun and the meter ticker misses its 2 s deadline.
# Upstream already ignores their sibling, the_limiter_keeps_two_full_decks_under_its_ceiling (dc6ba4b1).
RB_LITE_TEST=1 cargo test --workspace -- \
  --skip the_limiter_off_leaves_the_clamp_to_flat_top_the_sum \
  --skip paused_scrubs_on_either_deck_keep_emitting_audio_peaks_until_released
