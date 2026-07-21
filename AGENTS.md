@RTK.md

# The Usual — Agent Guide

The Usual is a pixel-art village-life simulation. The player manages a newcomer
household through a queue of ordinary orders; every resident, including the
household, remains autonomous.

## Project rules

- Read and update `pasm/spec/` before or alongside every structural or gameplay
  change. The model is the project's source of truth.
- Record an accepted design or technical choice in
  `pasm/spec/core/decisions.yaml`.
- The deterministic simulation belongs in `crates/village_sim`; presentation,
  input, UI, and audio belong in `crates/village_game`.
- Player-facing prose belongs in authored content, never in simulation logic.
- Run `uv run pasm validate`, `cargo fmt --all --check`, `cargo clippy
  --workspace --all-targets -- -D warnings`, and `cargo test --workspace`
  before committing relevant work.

## Current implementation target

The first executable checkpoint is the headless **Cottage Contention** fixture:
two newcomers traverse the two-storey cottage, contend for an object, execute
and cancel an order safely, and emit asserted semantic events.
