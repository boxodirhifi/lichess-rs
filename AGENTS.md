# AGENTS.md — lichess-rs

> Guidance for AI agents and human contributors working on this codebase.
> Read this **before** making changes. The architecture decisions here are intentional.

## Project Goal

Build a **lightweight, native Lichess desktop client** inspired by Alacritty:
minimal, fast, polished, and resource-efficient. It should feel like a native app,
not a web page wrapped in a browser.

### Hard Constraints (do NOT violate)
- **No heavyweight GUI frameworks.** No Qt, GTK, Electron, WebView, or similar.
  All rendering is done through our own lightweight UI layer on top of `wgpu`.
- **Wayland-first on Linux.** The app must work correctly on Wayland compositors.
- **Resource-efficient.** Avoid heavy async runtimes and unnecessary dependencies.
  The app should use ~0% CPU when idle and stay lean.
- **Modular architecture.** Each concern lives in its own crate. The chess core is
  independent of UI and Lichess networking.

## Architecture

Workspace of six crates. Respect the dependency direction (arrows point to "depends on"):
client ──▶ renderer ──▶ chess
│ │
├──▶ ui ───────┤
├──▶ platform
└──▶ lichess

| Crate | Responsibility |
|---|---|
| `chess` | Pure chess logic: board model, FEN parsing, move generation, legality, SAN parsing. **No UI, no networking.** Fully unit-tested. |
| `renderer` | `wgpu` rendering: quad pipeline (squares), text/glyph pipeline (text atlas + piece glyphs). Knows about `chess` types but not `ui`. |
| `ui` | Layout & interaction logic: board squares, selection, move targets, promotion menu. Produces render-ready geometry. |
| `platform` | Window creation & Wayland/X11 integration via `winit`. |
| `lichess` | Lichess API: REST (`ureq`) + real-time NDJSON streaming. Returns plain data; never touches rendering or the board. |
| `client` | The binary. Owns the event loop, app state, and wires everything together. |

## Key Design Decisions (the "why")

### 1. No tokio — blocking thread + EventLoopProxy
We deliberately do **not** use `tokio` or a heavy async runtime. We have exactly
one background task (the Lichess stream), so a single `std::thread` running blocking
`ureq` is simpler, leaner, and just as correct. The thread pushes updates to the
winit event loop via `EventLoopProxy` (thread-safe). **Do not introduce an async
runtime unless we genuinely need many concurrent connections.**

### 2. NDJSON streaming, not WebSocket
Lichess's public real-time API is NDJSON streaming. The WebSocket is internal and
undocumented. We stream `GET /api/tv/feed` line-by-line on the background thread.
Stick with NDJSON unless a feature specifically requires the WebSocket.

### 3. Event-driven rendering (not a render loop)
The app only redraws when state **actually changes** (a move arrives, the user
interacts, the window resizes) — including the one visible change that happens
on a timer: while spectating a live game whose clocks are still running,
`App::next_clock_tick()` schedules a redraw roughly every second via
`ControlFlow::WaitUntil` so the clocks visibly tick between server updates.
That is still event-driven (one scheduled redraw per state change), not a
render loop. Request redraws only in response to real changes. When idle,
CPU usage should be ~0%.

### 4. Clocks trust the server (no client interpolation)
For spectating, we display the exact clock values the server sends rather than
interpolating between events. Interpolation causes visible jitter/snaps in fast
formats (e.g. 1+0 bullet) because network latency makes our estimate drift from
the server's truth. Keep clocks server-authoritative.

### 5. Clocks freeze on game over
`LiveGameInfo.game_over` is set when the position is terminal or a flag drops.
When true, elapsed time is not subtracted from clocks so they don't drain forever.

### 6. Custom text rendering via glyph atlas
We rasterize glyphs (ASCII + chess piece Unicode) into a single GPU texture atlas
using `ab_glyph` and draw them as textured quads. This powers both text and pieces.
Don't reach for an external text/GUI library — extend the existing pipeline.

### 7. Board coordinates are drawn inside edge squares (no outer gutter)
The board is 80% of the smaller window dimension. File letters (a–h) and rank
numbers (1–8) are drawn *inside* the corner squares of the h-file and first
rank (Lichess-style), tinted with the opposite colour of the square they sit
on (see `coord_color` in `client`) so they always contrast. Because no outer
gutter is reserved, the promotion menu may freely dock in the margin outside
the board. If the board ratio changes, keep `UI::layout()` and the
coordinate-gutter assumption in sync.

## Coding Conventions

- **Complete files over patches.** When modifying, provide the full file so changes
  are unambiguous and easy to apply.
- **Keep crates decoupled.** Don't add cross-crate dependencies that violate the
  diagram above.
- **Test the chess core.** Any new chess logic needs unit tests in `crates/chess`.
- **No `unwrap()` in hot paths.** Prefer graceful degradation; `unwrap` is acceptable
  only where failure truly means the app cannot continue.
- Match existing style: 4-space indent, `snake_case`, concise but clear names.

## How to Build / Run / Test

```bash
cargo build            # build
cargo run              # run the client
cargo test             # run all tests (chess logic is well-covered)


The app opens a window, connects to Lichess TV, and streams a live game.
Press P → local play mode
Press L → live spectator mode
Roadmap (order matters)
Completed:
Real-time NDJSON streaming (replaced polling)
Clocks + player names/ratings
Pawn promotion menu (interactive, tested)
CPU fix: event-driven rendering
Board coordinates (a–h, 1–8)
Draw rules: threefold repetition, 50-move rule, insufficient material, halfmove/fullmove counters + `to_fen`
Up next:
Interaction polish
Actual playable games (Lichess API token auth + Board API)

Rules for Agents

1. Read this file first. Do not reverse or "simplify" the architectural decisions above.
2. Prefer small, testable increments. Each change should build and pass cargo test.
3. Don't add dependencies without a strong reason and alignment with the lightweight philosophy.
4. Preserve the crate boundaries. If a change seems to require coupling two crates, reconsider the design.
5. Update this file if you change architecture, add a crate, or complete a roadmap item.
