# The Usual

A pixel-art village-life simulation about a newcomer household trying to get
through the day, keep a home in order, and become somebody whose presence
matters in a slightly timeless British village.

The player queues, promotes, forces, and cancels tasks for their household.
Those tasks use the same autonomous planning, conducts, smart objects, and
capability arbitration as everyone else.

## Workspace

| Crate | Role |
| --- | --- |
| `village_sim` | Headless deterministic simulation: world, AI, plans, events, saves, and scenario tests. |
| `village_game` | Bevy desktop client: rendering, camera, input, UI, and audio. |

Authored RON content lives under `assets/content/` and is shared by both crates.

## Development

```powershell
uv sync --group dev
uv run pasm validate
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Architecture model

`pasm/spec/` is the project-owned PASM model. It holds the foundation,
accepted decisions, Cottage Arrival MVP, milestones, roadmap, and setting
guide. Update it before or alongside any system change.
