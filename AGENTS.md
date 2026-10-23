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
| `renderer` | `wgpu` pipelines: quads (squares/menus), glyph atlas (text), SVG piece atlas (cburnett set). |
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
5. **Pieces are real SVG piece sets (cburnett), not glyphs.** The 12 cburnett
   SVGs are bundled under `crates/renderer/assets/cburnett/` (`include_str!`) and
   rasterized once at renderer init with pure-Rust `resvg` (no fonts needed:
   `default-features = false`), packed into one `Rgba8UnormSrgb` texture atlas
   (4×3 cells of 256 px), and drawn through `PiecePipeline` as straight-alpha
   blended textured quads — no tint, no stamped outline; the SVG carries the
   colors. Board pieces and promotion-menu pieces share this path
   (`PieceElement.role/color` index the atlas). The `ab_glyph` pipeline stays
   for text only (coordinates, names, clocks, banner).
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
- [x] **Shakmaty migration** (Stages 0–3 done; spec below)
- [x] Real-time NDJSON streaming (replaced polling)
- [x] Clocks + player names/ratings (+ freeze on game over)
- [x] Pawn promotion menu (interactive, tested)
- [x] Event-driven rendering (CPU fix)
- [x] Board coordinates (in-square, Lichess-style)
- [x] Flat rendering + solid piece outlines
- [x] Spectating all Lichess TV variants (`Stage 3`: channel → shakmaty variant)
- [x] SVG piece sets (cburnett set via `resvg`, GPL-compatible) — bundled SVGs
  rasterized at init into a wgpu atlas, replacing glyph pieces

Known limitations:
- **No threefold-repetition tracking.** shakmaty positions are stateless (no move
  history), so a threefold draw can only be detected with a history tracker (e.g.
  a repetition hash per position). The 50-move banner *is* tracked via
  `halfmoves() >= 100`.

Next:
- [ ] Finish playable online games. Scaffolding already in the client:
  token auth (`~/.config/lichess-rs/token`, `Mode::Play`), challenge creation +
  Board API game stream (`stream_board_game`), move posting (`play_board_move`),
  UCI replay of the server move list via `replay_uci`. What remains is
  challenge UX (opponent/clock/time-control setup), takebacks/claims, and the
  swap from "echo the server" latency smoothing.
- [ ] Variant *play* (Crazyhouse drops etc.) — spectate-only today.
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

### Stage 2 — all state goes shakmaty; delete `crates/chess` ✅
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

### Stage 3 — variants light up ✅
- `lichess` worker: streams one TV channel at a time from the *per-channel* feed
  `/api/tv/{channel}/feed` (`?channel=` on `/api/tv/feed` is ignored upstream) and
  passes the channel key through `NetEvent::{FeaturedGame,FenUpdate}`. The worker
  rotates through `TV_CHANNELS` (`best`, `atomic`, `horde`, `crazyhouse`,
  `kingOfTheHill`, `threeCheck`, `antichess`, `racingKings`, `chess960`); each
  stream is held open for `TV_CHANNEL_DWELL` (30 s) via an overall ureq timeout —
  the deadline surfaces as a read error and is reported as `Ok(())` so one
  blocking thread can rotate without an async runtime.
- `client`: `position_from_fen(channel, fen)` resolves the channel key with
  `Variant::from_ascii` and builds the concrete position via the per-variant
  `FromSetup` constructors (`Atomic::from_setup`, `Horde::from_setup`, …),
  wrapped in `VariantPosition`. Chess960 maps to `Variant::Chess` with
  `CastlingMode::Chess960`. Only the "standard" channel (online-play initial
  FENs) is intentionally parsed as plain `Chess`.
- Banner: `App::game_banner(board)`, shared by local play and TV. Variant endings
  first via `is_variant_end()` + `variant_end_text()` ("King in the centre",
  "King exploded", …), then checkmate/stalemate/insufficient-material, then the
  50-move rule.
- Spectate-only: clicks do nothing in Live mode, and Crazyhouse `Move::Put`
  (drops) has no origin square, so origin-square filters already exclude it —
  no variant play.
- Tests: horde + atomic + crazyhouse TV FENs parse into their variants with
  exact piece counts, `legal_moves()` runs without panicking, and pockets are
  visible; an adjacent-kings FEN parses as Atomic but fails as standard chess;
  the channel→variant map and the KOTH variant-end banner text are covered.
- Gate: `cargo test` green; `cargo run` cycles variant TV games without crashing
  (verified end-to-end through all nine channels).

## Rules for Agents

1. **Read this file first.** Do not reverse or "simplify" the locked decisions above.
2. Execute **one migration stage at a time**; each ends with green tests + a commit.
3. **No new dependencies** beyond those listed here without discussion.
4. **Never reintroduce `crates/chess`** or hand-rolled chess rules.
5. **Update this file** when a stage completes or the roadmap changes.
```
