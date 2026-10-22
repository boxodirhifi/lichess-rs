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
- **Wayland-first on Linux.**
- **Resource-efficient.** ~0% CPU when idle. No async runtime. Lean dependency tree.
- **Modular architecture.** Lichess networking is decoupled from rendering; chess
  rules are delegated entirely to `shakmaty` (see below).

## Architecture

Workspace crates (post-migration):

| Crate | Responsibility |
|---|---|
| `client` | The binary. Event loop, app state, wires everything together. |
| `renderer` | `wgpu` pipelines: quads (squares/menus), glyph atlas (text + piece glyphs today). |
| `ui` | Layout & interaction geometry: squares, selection, move targets, promotion menu, coordinates. |
| `platform` | Window creation & Wayland/X11 integration via `winit`. |
| `lichess` | REST (`ureq`) + real-time NDJSON streaming. Returns plain data (FEN/SAN strings, player info). Never touches `shakmaty` or rendering. |
| ~~`chess`~~ | **Deleted** during the shakmaty migration. Do not reintroduce. |

Dependency direction: `client` → {`renderer`, `ui`, `platform`, `lichess`};
`renderer`/`ui` → `shakmaty` (types only).

## Chess Core: shakmaty, first-class

- **All chess rules come from `shakmaty` 0.30 with `features = ["variant"]`.**
  No wrappers, no translation layer, no local aliases: code uses shakmaty types
  directly — `Square`, `Color`, `Role`, `Piece`, `Move`, `Chess`, `VariantPosition`,
  the `Position` trait.
- Piece types are **`shakmaty::Role`** (there is no `PieceKind`). Squares are
  `shakmaty::Square` (`#[repr(u8)]`, `A1 = 0 … H8 = 63`); numeric index via
  `sq as u8`; file/rank via `.file()` / `.rank()` or index math (`idx % 8`, `idx / 8`,
  rank 0 = bottom of the board).
- **Game end is matched on shakmaty's `Outcome` / `is_checkmate()` / `is_stalemate()`
  directly at the use site.** There is no local `GameStatus` enum.
- **Variants:** positions are held as `shakmaty::VariantPosition` so any Lichess
  variant parses and renders. For TV, the variant is determined by the channel key
  (the stream worker must pass it through `NetEvent`). Spectating all variants is in
  scope; variant *play* (e.g. Crazyhouse drops in the UI) is deferred.
- **Never hand-roll** move generation, legality, SAN/FEN parsing, or check detection.
  Use `shakmaty::{san::San, uci::Uci, fen::Fen, Position}`.

## Error Handling

- Library crates (`lichess`, future libs): **`thiserror`** for typed errors; wrap
  upstream errors (`ureq`, `serde_json`, `shakmaty`) with `#[from]`.
- The `client` binary: **`anyhow`** at top level where typed matching isn't needed.
- No hand-written `Display`/`Error` impls for new error types.

## Key Design Decisions (the "why")

1. **No tokio.** One `std::thread` + blocking `ureq` for the stream; results pushed
   into winit via `EventLoopProxy`. Do not add an async runtime unless we genuinely
   need many concurrent connections.
2. **NDJSON streaming, not WebSocket.** `GET /api/tv/feed` line-by-line. Lichess's
   WebSocket is internal/undocumented.
3. **Event-driven rendering.** `request_redraw()` only when visual state actually
   changes (network event, input, resize, first frame). Never unconditionally at the
   end of `window_event` — that caused a 100%-CPU unbounded loop before.
4. **Clocks trust the server.** Display exact `wc`/`bc` values; freeze via
   `LiveGameInfo.game_over` when the game ends. No client-side interpolation.
5. **Pieces are Unicode glyphs** rasterized via `ab_glyph` into a GPU atlas, drawn
   with a solid 8-offset outline (Lichess/Chess.com-style contrast). Planned upgrade:
   real SVG piece sets via `resvg` (separate increment — see Roadmap). Keep the glyph
   path until then.
6. **Coordinates are in-square, Lichess-style:** files `a–h` bottom-left of rank-1
   squares, ranks `1–8` top-right of h-file squares, tinted with the opposite color
   of their square. Board = 80% of the min window dimension. No outer gutter.
7. **Text is flat.** No drop shadows, except the game-over banner which overlays the
   board (`TextElement.shadow = true` only there).

## Coding Conventions

- **Complete files over patches** when modifying.
- Keep crate boundaries; `lichess` stays shakmaty-free and render-free.
- Behavior changes need tests. Chess-rule behavior is guarded by integration tests
  in `crates/client/tests/` plus a perft sanity check.
- No `unwrap()` in hot paths.
- 4-space indent, `snake_case`.

## Build / Run / Test

```bash
cargo build && cargo test && cargo run
```

App behavior: opens on live Lichess TV. `P` = local play, `L` = live, `Esc` = close
promotion menu.

## Roadmap

Completed:
- [x] Real-time NDJSON streaming (replaced polling)
- [x] Clocks + player names/ratings (+ freeze on game over)
- [x] Pawn promotion menu (interactive, tested)
- [x] Event-driven rendering (CPU fix)
- [x] Board coordinates (in-square, Lichess-style)
- [x] Flat rendering + solid piece outlines

In progress:
- [ ] **Shakmaty migration** (Stages 0–1 done; Stages 2–3 below)

Next:
- [ ] SVG piece sets via `resvg` (cburnett set, GPL-compatible) — own increment
- [ ] Playable games (Lichess Board API + token auth; shakmaty gives UCI for free)
- [ ] Theming — and only then `notify` for hot-reload

## Migration Spec: shakmaty (execute in order, one commit per stage)

### Stage 0 — prep ✅
- Workspace `Cargo.toml`: add to `[workspace.dependencies]`:
  `shakmaty = { version = "0.30", features = ["variant"] }`, `thiserror = "2"`,
  `anyhow = "1"`.
- `lichess`: convert its hand-rolled `Error` enum to a `thiserror` derive.

### Stage 1 — renderer speaks shakmaty (rendering only) ✅
- `renderer/Cargo.toml`: replace `chess` with `shakmaty`.
- `text_pipeline.rs`: `piece_char(Role)`, `piece_tint(Color)`, `piece_outline(Color)`,
  `push_piece(Role, Color, …)`; `prepare` iterates `shakmaty::Square::ALL` with
  `pos.board().piece_at(sq)`; screen coords from index (`idx = sq as u8`,
  `file = idx % 8`, `rank = idx / 8`, screen row = `7 - rank`).
- `PieceElement`/`Scene` use `Role`/`Color`; the board field becomes
  `&shakmaty::VariantPosition`.
- `client` bridges its old `chess::Board` → shakmaty **for the render call only**
  (throwaway code, removed in Stage 2).
- Gate: `cargo test` green; `cargo run` renders identically to before.

### Stage 2 — all state goes shakmaty; delete `crates/chess`
- `ui`: every `Square`/`Color`/`PieceKind` becomes shakmaty `Square`/`Color`/`Role`;
  `square_at` builds a `Square` from the computed index; `PROMOTION_KINDS: [Role; 4]`;
  `promotion_at -> Option<Role>`; port its tests.
- `client`: `board`/`live_board: VariantPosition`; `side: Color`; game-end matched on
  `Outcome`/`is_checkmate()`/`is_stalemate()` at the banner; clicks use
  `pos.legal_moves()` + `play_unchecked`; promotion detected via
  `Move::Normal { promotion: Some(_), .. }`; FEN via `fen::Fen`; turn via `pos.turn()`.
- **Delete `crates/chess`** (directory, workspace member, all `path` deps).
- Port the old rule tests into `crates/client/tests/rules.rs` against shakmaty
  (pin, en-passant-exposes-king, castling-through-check, SAN replay of the captured
  live game) **plus** a perft sanity check: start position, depth 4 == 197281
  (use `shakmaty::perft` if exposed, else a 5-line recursion over `legal_moves()`).
- Gate: `cargo test` green; `cargo run` plays locally and streams live TV.

### Stage 3 — variants light up
- `lichess` worker: pass the channel/variant name through `NetEvent`; client builds
  the matching variant position from the FEN; stop filtering to standard-only channels.
- UI: ignore variant-specific move kinds (drops) for now — spectate only.
- Tests: parse a horde FEN and an atomic FEN; `legal_moves()` doesn't panic; piece
  counts sane.
- Gate: `cargo test` green; `cargo run` cycles variant TV games without crashing.

## Rules for Agents

1. **Read this file first.** Do not reverse or "simplify" the locked decisions above.
2. Execute **one migration stage at a time**; each ends with green tests + a commit.
3. **No new dependencies** beyond those listed here without discussion.
4. **Never reintroduce `crates/chess`** or hand-rolled chess rules.
5. **Update this file** when a stage completes or the roadmap changes.
```
