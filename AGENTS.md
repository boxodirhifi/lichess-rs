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
8. **Click-to-move and drag-to-move share one path.** Press runs the click handler,
   then arms a drag if the press selected a piece; release resolves a drag (only
   after the cursor left the click slop) through the same `handle_board_click`, so
   targets, promotion and clearing behave identically. The dragged piece is
   `Scene.dragged`: the piece pipeline skips its origin square and draws it last
   (only the text pass — the banner — comes after).
9. **The last-move highlight is recorded locally and derived in spectate.** `ui`
   holds `last_move: Option<(Square, Square)>` and tints both squares muted yellow,
   under the check → selected → target priorities. Local/Play record the move as it
   is applied (`mv.from()`, `move_destination(mv, pos)` so castling lands on the
   king's destination). TV only receives FENs, so
   `derive_played_move(prev, next)` replays every `prev.legal_moves()` on a clone and
   returns the single move whose `board()` matches `next.board()`. No match (missed
   events, channel rotation) clears the tint rather than guessing; `FeaturedGame`,
   mode switches and new games clear it. Crazyhouse drops have no origin square, so
   they are never highlighted.
10. **Captured material is a stateless count-diff; history is one SAN list.** For
   each `(Role, Color)` the tray shows `max(0, start_count − board_count)`, with
   `start_count` read from the variant's own default position (`Chess`, `Horde`, …),
   so a reconnect can never leave stale pieces. Standard values (P1 N3 B3 R5 Q9) give
   the lead, appended to the leading side's clock text (`"05:00 +14"`). Icons are
   ordinary `Scene.pieces` at 0.4× square, started after the measured player name
   (`Renderer::text_width`) and truncated by `tray_fit` before the right-aligned
   clock. Promotions make the counts approximate (see Known limitations).
   `App.history` keeps SAN (`San::from_move`) for the last ~30 plies and renders a
   right-side column only when the board leaves ≥300 px of spare width — no
   scrolling, no input, no PGN export.
11. **Crazyhouse pockets are shakmaty's; drops are ordinary moves.** Pocket contents
   come straight from `VariantPosition::pockets()` (authoritative per-side
   `ByRole<u8>`, no captured-material count-diff). The client groups them
   pawn→queen and hands `ui.set_pocket` only what to draw; `ui` lays out two rows
   just outside the player bars (Black above, White below, board-left aligned,
   0.45× square icons, `xN` badges) so names, clocks and the move-history column
   never collide; a row that runs past the board edge is truncated with `…`. Other
   variants have no pockets, so the rows stay empty — the pocket UI is
   Crazyhouse-only. Only the side to move can drop: Live is display-only (dimmed at
   0.5 alpha), Local enables the side-to-move pocket, Play only `my_color` while
   the game runs and it is my turn. Clicking a usable pocket piece stores
   `ui.selected_pocket_piece` (mutually exclusive with `selected`) and highlights
   the legal landing squares from `legal_moves()` filtered to `Move::Put { role, .. }`
   in the normal TARGET tint; dropping finds the matching `Move::Put { role, to }`
   and goes through `apply_move` like any other move (no last-move origin, SAN is
   e.g. `Q@d4`). A pocket piece can also be dragged: `ui.dragging_pocket` is
   exclusive with a board drag, the lifted piece follows the cursor through
   `Scene.dragged` with `DragSource::Pocket` (the board hides nothing), and
   releasing on a target drops it while releasing anywhere else cancels.
   `PieceElement.alpha` drives the dimming (renderer multiplies it into the
   sampled SVG alpha only).

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
promotion menu. Pieces move by click-click or drag-and-drop (press a piece, drag to
a legal target, release); a press/release without moving stays a plain click. In
Crazyhouse, clicking a pocket piece highlights its legal drops (click one, or drag
the piece out and release there); dimmed pocket rows are display-only.

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
- [x] Drag-to-move alongside click-to-move (drag origin lifted off the board,
  rendered under the cursor; promotion still picked by click)
- [x] **Phase A polish**: last-move highlight in Local/Live/Play (derived from FEN
  diffs while spectating), captured-piece trays + `+N` material next to the leading
  clock, and a SAN move-history column when the window leaves ≥300 px of spare width
- [x] **Phase B — Crazyhouse pockets**: authoritative `pockets()` rows outside the
  player bars (grouped pawn→queen, `xN` badges, dimmed when not droppable),
  click-to-select + drag-to-drop through `Move::Put`, playable in Local/Play and
  display-only in Live; non-Crazyhouse positions render no pockets

Known limitations:
- **No threefold-repetition tracking.** shakmaty positions are stateless (no move
  history), so a threefold draw can only be detected with a history tracker (e.g.
  a repetition hash per position). The 50-move banner *is* tracked via
  `halfmoves() >= 100`. `App.history` stores rendered SAN strings for the panel, not
  positions, so it can't drive repetition detection.
- **Promotions make the captured count-diff approximate.** A promoted pawn is
  missing from the board, so the tray reads it as captured, and the promoted piece
  reads as a captured enemy piece. Spectate mode has no true move history to correct
  this (moves are derived from FEN diffs), so expect up to one pawn unit of drift per
  promotion; online play is close but not exact either.

Next:
- [ ] Finish playable online games. Scaffolding already in the client:
  token auth (`~/.config/lichess-rs/token`, `Mode::Play`), challenge creation +
  Board API game stream (`stream_board_game`), move posting (`play_board_move`),
  UCI replay of the server move list via `replay_uci`. What remains is
  challenge UX (opponent/clock/time-control setup), takebacks/claims, and the
  swap from "echo the server" latency smoothing.
- [ ] Variant *play* beyond Crazyhouse drops (Atomic, Horde, three-check, …) —
  spectate-only today.
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
