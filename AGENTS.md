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
| `renderer` | `wgpu` pipelines: quads (squares/menus), glyph atlas (text), SVG piece atlas (four bundled sets). |
| `ui` | Layout & interaction geometry: squares, selection, move targets, promotion menu, challenge panel, action buttons, coordinates. |
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
5. **Pieces are real SVG piece sets, not glyphs.** The 12 SVGs of each bundled
   set live under `crates/renderer/assets/<set>/` (`include_str!`) and are
   rasterized with pure-Rust `resvg` (no fonts needed: `default-features = false`),
   packed into one `Rgba8UnormSrgb` texture atlas (4×3 cells of 256 px), and
   drawn through `PiecePipeline` as straight-alpha blended textured quads — no
   tint, no stamped outline; the SVG carries the colors. Board pieces and
   promotion-menu pieces share this path (`PieceElement.role/color` index the
   atlas). The `ab_glyph` pipeline stays for text only (coordinates, names,
   clocks, banner). The set is runtime-swappable (decision 13).
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
12. **Atomic blasts are a transient visual, never board state.** A capture in
   `Variant::Atomic` starts an `App.explosion` (blast squares + ghost snapshots +
   `Instant`). Local derives it in `apply_move` from the pre-move clone, Live in
   `note_live_move` from the FEN diff, Play from `derive_played_move` on the board
   stream's echoed move list; background streams are gated on the current mode so
   a TV game can never animate a local game (and vice versa). The radius is the
   capture square plus its Moore neighbourhood (`explosion_squares`, clipped at
   the edges). The flash is `0.8 * (1 - t²)` over 500 ms blended into the **base**
   square colour inside `ui` (`set_explosion`/`clear_explosion`), so
   check → selected → target → last-move highlights still win over it. Ghosts are
   exactly the pieces shakmaty's board diff says disappeared — explosion-immune
   pawns and the mover's origin never fade — drawn as ordinary `Scene.pieces`
   after the captured trays (drags still render on top); the destination ghost
   fades in 200 ms, the others over 500 ms. `tick_explosion()` runs only on the
   redraw path and keeps `needs_redraw` true while the effect lives, then the loop
   drops back to `Wait` (no timer, no idle work). Mode switches and new featured
   games clear the blast; other variants never set it.
13. **Theming is one optional TOML file, hot-reloaded by a blocking watcher.**
   `~/.config/lichess-rs/config.toml` (`$XDG_CONFIG_HOME` honoured, directory
   created 0700 at startup) holds `[theme] board = "…"` and `pieces = "…"`. A
   missing file is created 0600 from the documented defaults (`brown`,
   `cburnett`; decision 14); malformed or unknown names → warn and keep the
   running selection (startup falls back to the defaults). `ui::BoardTheme` owns
   the four palettes (`light`, `dark`, `brown`, `wood`) and `apply_colors` reads
   them, so every square tint, selection, explosion blend and coordinate colour
   (the opposite-square tint, now theme-aware in the client) follows along.
   `renderer::PieceSet` owns the four bundled sets; `Renderer::reload_pieces`
   re-rasterizes the set into the **same** atlas texture in place, so the bind
   group and view stay valid and no pipeline is rebuilt. A `notify` watcher runs
   on its own `std::thread` (same pattern as the TV worker) watching the config
   *directory* — editors save via rename, which would kill a file watch — filters
   events to `config.toml`, coalesces a 120 ms burst, and sends
   `NetEvent::ConfigChanged` through the `EventLoopProxy`. The event loop reloads
   the file, applies only what changed, and requests one redraw; the thread then
   blocks in `mpsc::recv` again, so idle CPU stays at zero and there is no
   polling timer anywhere.
14. **Secrets and config are owner-only; shared permissions are warned about,
   never auto-fixed.** The client creates `~/.config/lichess-rs/` with mode 0700,
   and writes the empty token placeholder (from the auth flow) and the default
   `config.toml` through `permissions::write_private_file`, which opens with 0600
   and then re-applies the mode with `set_permissions` (the open mode is filtered
   through the umask, and a pre-existing file keeps its old bits). Existing files
   are the user's: startup and every `ConfigChanged` call
   `permissions::warn_if_shared_readable`, which warns when
   `mode & 0o044 != 0` (group- or world-readable) and suggests `chmod 600`.
   Non-Unix platforms skip the POSIX bits with a debug log — Windows uses ACLs,
   not modes. No new dependencies: `std::fs` plus `std::os::unix::fs::PermissionsExt`
   behind `#[cfg(unix)]`.
15. **Online play is a small explicit state machine over a pinned, validated
   Board API client.** `lichess::ApiClient` is hard-wired to
   `https://lichess.org` in production (the `with_base`/local constructor is
   `#[cfg(test)]`) and keeps ureq's default TLS validation. It retries only
   HTTP 429: three retries, exponential 250 ms → 500 ms → 1 s (capped at 5 s),
   honouring the `Retry-After` header. The token is a `lichess::Token` newtype
   whose `Debug`/`Display` print `[redacted]`; the only reader is
   `LICHESS_API_TOKEN` or the 0600 file, and every worker error that can reach
   the banner passes through `lichess::redact(message, token)`. Inputs are
   validated before any request (username 2–30 ASCII alphanumeric/`_`/`-`,
   clock limit 0/15/30/45/90 or a multiple of 60 up to 10 800 s, increment
   0–60 s, game id 4–16 alphanumeric, UCI `e2e4`/`e7e8q`), and moves are
   additionally checked by shakmaty before they are posted. Responses are
   deserialized into typed structs: malformed JSON is `Error::Parse`, a
   malformed NDJSON frame is skipped so it cannot kill a live game. Client
   state is the explicit enum `Idle → Challenging → WaitingAccept → Active →
   Finished`, with `Failed` reachable from any step (it keeps the last board
   visible but unplayable). Challenge creation sets `keepAliveStream=true`, so
   the POST is NDJSON: the first line is the challenge object and the stream
   stays open (server pings every 15 s) until `{"done":…}` reports
   accepted/declined/canceled. That both keeps the realtime challenge past
   Lichess's 20 s unaccepted expiry and removes the old 404/`/show` polling
   loop; a post-acceptance 404 is retried briefly while the game is created.
   The endpoint contract (`gameFull`/`gameState` shapes) lives on
   `lichess::api`. API tests run against a local `std::net::TcpListener`, so
   the suite stays offline and needs no mock crate.
16. **Challenges and in-game actions are UI geometry in `ui`, HTTP in
   `lichess`, decisions in `client`.** `C` opens a modal `ChallengePanel`
   (opponent field, six clock presets, three colour choices, Create/Cancel)
   owned by `ui` (layout, hit-testing, selection state) and rendered by the
   client; typing accepts only the username alphabet (30 max), Enter submits,
   Escape/outside clicks dismiss, and invalid input keeps the panel open with
   a red status line (the token hint lives there too). The panel's quads use a
   **second quad pass drawn after pieces** (`Scene.overlay_quads`), so the
   opaque dialog covers the board instead of sitting under its pieces; while
   it is open it also owns the whole text pass (no coordinates/bars/history
   showing through). In an active game a Takeback / Draw / Resign row sits in
   the bottom margin; `ui::ControlKind` hit-tests, `client::send_control`
   posts the documented endpoint on a worker thread, and the reply is
   optimistic (`"Draw offer sent"`) with a redacted `OnlineNotice` on failure.
   The row exists only in `OnlineState::Active` with `!game_over`; finished,
   failed and inactive states keep the final board but cannot send. `renderer`
   gained only the overlay pass (same sphere pipeline, no new dependency).
17. **Online moves are optimistic and reconciled against the echoed move
   list.** Play applies the move to `OnlineGame.board` immediately, records
   `PendingMove { uci, index }` (index = plies already confirmed) and posts in
   the background, so the piece never waits for a round trip. `OnlineGame`
   keeps the initial FEN, the full `server_moves`, and a `server_board`
   rebuilt from them on every frame: a duplicated or missed `gameState` can
   never corrupt the authoritative position. Reconciliation confirms when
   `moves[pending.index] == pending.uci` (adopt the server board and append
   only the unseen tail, so SAN/highlight/blast are not duplicated), keeps
   the optimistic board while the stream has not caught up, and rolls back to
   `server_board` — popping the optimistic SAN and clearing the highlight —
   when the server moved past that index with a different move. A failed POST
   emits `OnlineMoveRejected` and rolls back at once; if the stream later
   confirms the move anyway, normal tail replay re-advances and adds the SAN
   exactly once.
18. **Online failures are short, readable, and never overflow.** The client's
   `online_error` maps typed `lichess::Error`s to one-line user messages
   (401 → token rejected / check the token file, 403 → missing `board:play`,
   404 → not found, 429 → rate limited, 400/5xx/transport/parse → concise
   fallbacks) while the redacted full detail goes to the log only.
   `Token::new` also strips a pasted `Bearer ` prefix and matching quotes. The
   renderer banner greedily wraps at 86% of the window width, shrinks from
   `1.1 × square` (capped at 64 px) down to 16 px across at most four lines,
   and ellipsizes the last line as a last resort, so even a long error stays
   on screen; short game-over banners render exactly as before.

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

App behavior: opens on live Lichess TV. `P` = local play, `L` = live, `C` =
open the online challenge panel (Board API; type the opponent, pick a clock
and colour, Enter to send, Esc to cancel), `Esc` = close promotion menu. An
active online game shows Takeback / Draw / Resign buttons under the board.
Pieces move by
click-click or drag-and-drop (press a piece, drag to a legal target, release); a
press/release without moving stays a plain click. In
Crazyhouse, clicking a pocket piece highlights its legal drops (click one, or drag
the piece out and release there); dimmed pocket rows are display-only. Theming is
driven by `~/.config/lichess-rs/config.toml` and applies live on save.
`~/.config/lichess-rs/` is created 0700, the token and config files are written
0600, and the app warns (without auto-fixing) if either file is group- or
world-readable.

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
- [x] **Phase B (extended) — Atomic explosions**: Atomic captures flash the 3×3
  blast radius for 500 ms (ease-out, under the interaction highlights) and fade the
  pieces shakmaty actually removes (the destination faster, 200 ms); drives Local,
  Live and Play, cleared on mode/game switches and absent in every other variant
- [x] **Phase C — Theming**: optional `~/.config/lichess-rs/config.toml`
  (`[theme] board`, `[theme] pieces`), four board palettes with theme-aware
  coordinates, four bundled SVG sets (`cburnett`, `merida`, `alpha`, `leipzig`),
  and a blocking `notify` watcher that hot-reloads both while the app runs
- [x] **Security hardening — owner-only token/config permissions**: the config
  directory is created 0700; the token placeholder and default config are 0600
  (Unix); startup and hot-reload warn when either file is group/world-readable
  (`mode & 0o044`), never auto-fixing the user's mode
- [x] **Online play Stage 1 — secure Board API client + state machine**: pinned
  `https://lichess.org` client with a local-test constructor, `Token` redaction,
  pre-flight validation, bounded 429 backoff, typed `thiserror` errors, and the
  explicit `Idle/Challenging/WaitingAccept/Active/Finished/Failed` client enum
  (the final board stays visible after a failure but is not playable)
- [x] **Online play Stage 2 — challenge panel + game controls**: modal
  opponent/clock/colour panel (`C`), validated submission, an overlay quad pass
  so the panel covers the board, and Takeback / Draw / Resign buttons wired to
  `/api/board/game/{id}/{takeback/yes,draw/yes,resign}` with optimistic notices
- [x] **Online play Stage 3 — optimistic moves + reconciliation**: local board
  advance with `PendingMove`, authoritative rebuild from the echoed move list,
  confirm-by-UCI, rollback on rejection/failed POST, and no duplicate SAN,
  highlight or blast when frames are repeated or skipped

Known limitations:
- **A lost move POST response can make the piece blink.** If Lichess accepted
  the move but the reply never reached us, `OnlineMoveRejected` rolls the board
  back until the stream echoes the move and reconciliation re-advances it; the
  final position and SAN count stay correct.
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
- **The Atomic capturing piece does not ghost.** shakmaty removes it along with the
  captured piece, but it shares the destination square, so a second ghost there
  would overlap the captured one. The destination ghost shows the captured piece
  (200 ms) and the 500 ms flash covers the capturer's disappearance.

Next:
- [ ] Online play polish — an explicit draw/takeback offer banner and
  accept/decline handling from the stream (`gameState` currently only carries
  the result), plus claim-victory / abort endpoints.
- [ ] Variant *play* beyond Crazyhouse drops (Atomic, Horde, three-check, …) —
  spectate-only today.

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
