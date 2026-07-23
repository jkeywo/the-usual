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

## Web build

The Bevy client also builds to WebAssembly and is published to GitHub Pages by
`.github/workflows/pages.yml` on every push to `main`.

One-time setup: in the repository, open **Settings -> Pages -> Build and
deployment** and set **Source** to **GitHub Actions**. After the next push to
`main` the site is served at `https://<owner>.github.io/the-usual/`.

The browser has no filesystem, so on `wasm32-unknown-unknown` the client loads
its scenario content compiled in via `ScenarioContent::embedded_cottage_arrival`
(kept in step with the authored files by a test) and reads image, font, and
audio assets from a relative `assets/` path served next to the page.

Build and preview it locally:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126
cargo build --release -p village_game --target wasm32-unknown-unknown
wasm-bindgen --no-typescript --target web --out-dir dist --out-name village_game \
  target/wasm32-unknown-unknown/release/village_game.wasm
cp web/index.html dist/ && mkdir -p dist/assets && cp -r assets/client dist/assets/client
python -m http.server -d dist 8080   # then open http://localhost:8080
```

## Architecture model

`pasm/spec/` is the project-owned PASM model. It holds the foundation,
accepted decisions, Cottage Arrival MVP, milestones, roadmap, and setting
guide. Update it before or alongside any system change.
