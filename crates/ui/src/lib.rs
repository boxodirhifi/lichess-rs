use shakmaty::variant::VariantPosition;
use shakmaty::{Color, Position, Rank, Role, Square};

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: [f32; 3],
}

const LIGHT: [f32; 3] = [240.0 / 255.0, 217.0 / 255.0, 181.0 / 255.0];
const DARK: [f32; 3] = [181.0 / 255.0, 136.0 / 255.0, 99.0 / 255.0];
const SELECTED: [f32; 3] = [106.0 / 255.0, 170.0 / 255.0, 100.0 / 255.0];
const TARGET: [f32; 3] = [0.30, 0.55, 0.85];
/// Muted yellow over both squares of the last played move (Lichess-style).
const LAST_MOVE: [f32; 3] = [205.0 / 255.0, 178.0 / 255.0, 80.0 / 255.0];
const CHECK: [f32; 3] = [0.85, 0.25, 0.25];
/// Bright blast tint for Atomic explosions, blended over the base square
/// colour (and under every selection/target/check/last-move tint).
const EXPLOSION: [f32; 3] = [1.0, 1.0, 0.7];
const PANEL: [f32; 3] = [0.24, 0.23, 0.22];
const PANEL_CELL: [f32; 3] = [0.86, 0.82, 0.74];

/// Linear blend of two RGB colours; `t` is clamped, so callers can pass an
/// eased 0..1 flash strength straight through.
fn blend(from: [f32; 3], to: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        from[0] + (to[0] - from[0]) * t,
        from[1] + (to[1] - from[1]) * t,
        from[2] + (to[2] - from[2]) * t,
    ]
}

/// One entry of the promotion menu: where it sits and what it promotes to.
#[derive(Debug, Clone, Copy)]
pub struct PromotionChoice {
    pub rect: Rect,
    pub role: Role,
    pub color: Color,
}

/// One grouped Crazyhouse pocket entry. Crazyhouse pockets hold at most one
/// entry per role; the count rides in a small badge beside the icon.
#[derive(Debug, Clone, Copy)]
pub struct PocketPiece {
    /// Icon rectangle; the piece fills it.
    pub rect: Rect,
    pub role: Role,
    pub color: Color,
    pub count: u8,
    /// Whether this side may drop right now (side to move / our colour).
    pub interactive: bool,
}

impl PocketPiece {
    /// Pocket pieces that cannot be used are dimmed to signal that they are
    /// display-only.
    pub fn alpha(&self) -> f32 {
        if self.interactive {
            1.0
        } else {
            0.5
        }
    }

    /// `x2` style badge text, only when the role appears more than once.
    pub fn badge(&self) -> Option<String> {
        (self.count > 1).then(|| format!("x{}", self.count))
    }
}

#[derive(Default)]
pub struct UI {
    pub squares: Vec<Rect>,
    selected: Option<Square>,
    move_targets: Vec<Square>,
    /// Pocket piece picked for a drop; mutually exclusive with `selected`.
    selected_pocket_piece: Option<Role>,
    /// Grouped pocket pieces per colour, in tray order. Empty outside
    /// Crazyhouse, which is how the pocket UI stays variant-only.
    pocket_black: Vec<PocketPiece>,
    pocket_white: Vec<PocketPiece>,
    /// The row ran out of room and was cut short (rendered with an ellipsis).
    pocket_truncated: [bool; 2],
    /// X position just past the last pocket element, per colour.
    pocket_end: [f32; 2],
    /// Atomic blast squares and the current (already eased) flash strength.
    /// Blended into the base square colour only, so highlights stay on top.
    explosion: Vec<Square>,
    explosion_flash: f32,
    checked_king: Option<Square>,
    /// From/to squares of the last played move, tinted in both play and TV.
    last_move: Option<(Square, Square)>,
    /// Square the piece being dragged came from, while the mouse is down.
    dragging: Option<Square>,
    /// Role being dragged out of a pocket; mutually exclusive with `dragging`.
    dragging_pocket: Option<Role>,
    drag_origin: Option<(f32, f32)>,
    /// Whether the cursor has left the click slop since `start_drag`; a press
    /// and release without movement stays a plain click.
    drag_moved: bool,
    promotion: Option<Promotion>,
    screen_w: f32,
    screen_h: f32,
    board_px: f32,
    sq_size: f32,
    offset_x: f32,
    offset_y: f32,
}

/// An open promotion menu.
struct Promotion {
    from: Square,
    to: Square,
    color: Color,
    choices: Vec<PromotionChoice>,
}

/// The pieces offered on promotion, in menu order.
pub const PROMOTION_KINDS: [Role; 4] = [Role::Queen, Role::Rook, Role::Bishop, Role::Knight];

impl UI {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn layout(&mut self, screen_w: f32, screen_h: f32) {
        self.squares.clear();
        // 80% of the smaller dimension. Coordinates live inside the edge
        // squares (Lichess-style), so no outer gutter is needed.
        let board_px = screen_w.min(screen_h) * 0.80;
        let sq_size = board_px / 8.0;
        let offset_x = (screen_w - board_px) / 2.0;
        let offset_y = (screen_h - board_px) / 2.0;

        self.screen_w = screen_w;
        self.screen_h = screen_h;
        self.board_px = board_px;
        self.sq_size = sq_size;
        self.offset_x = offset_x;
        self.offset_y = offset_y;

        for r in 0..8 {
            for c in 0..8 {
                self.squares.push(Rect {
                    x: offset_x + c as f32 * sq_size,
                    y: offset_y + r as f32 * sq_size,
                    w: sq_size,
                    h: sq_size,
                    color: LIGHT,
                });
            }
        }
        self.apply_colors();
        self.relayout_promotion();
    }

    /// Re-place the promotion menu after a resize, keeping it beside the
    /// square the pawn is moving to.
    fn relayout_promotion(&mut self) {
        if self.promotion.is_none() || self.sq_size <= 0.0 {
            return;
        }
        let (from, to, color) = {
            let p = self.promotion.as_ref().unwrap();
            (p.from, p.to, p.color)
        };
        let choices = self.layout_promotion_choices(to, color);
        self.promotion = Some(Promotion { from, to, color, choices });
    }

    /// Lay out the four menu cells along the file of `to`, in the margin next
    /// to the rank the pawn is moving onto.
    fn layout_promotion_choices(&self, to: Square, color: Color) -> Vec<PromotionChoice> {
        let file = to.file().to_u32() as f32;
        let sq = self.sq_size;
        // Rank 8 is the top row of the screen, rank 1 the bottom one.
        let above = to.rank() == Rank::Eighth;
        let pad = sq * 0.12;

        // Vertical room outside the board. It is always at least one square
        // tall, and the menu must stay clear of it: board pieces are drawn on
        // top of quads, so an overlapping backdrop would look broken.
        let margin = if above {
            self.offset_y
        } else {
            self.screen_h - (self.offset_y + self.board_px)
        };
        let side = (margin - pad * 2.0).clamp(sq * 0.4, sq);
        let total = PROMOTION_KINDS.len() as f32 * side;

        // Centered on the file of the promoting pawn, clamped on screen.
        let x = (self.offset_x + (file + 0.5) * sq - total / 2.0)
            .clamp(pad, (self.screen_w - total - pad).max(pad));
        let y = if above {
            (self.offset_y - pad - side).max(pad)
        } else {
            (self.offset_y + self.board_px + pad).min((self.screen_h - pad - side).max(pad))
        };

        PROMOTION_KINDS
            .iter()
            .enumerate()
            .map(|(i, role)| PromotionChoice {
                rect: Rect {
                    x: x + i as f32 * side,
                    y,
                    w: side,
                    h: side,
                    color: PANEL_CELL,
                },
                role: *role,
                color,
            })
            .collect()
    }

    /// Open the promotion menu for a pawn moving from `from` to `to`.
    pub fn open_promotion(&mut self, from: Square, to: Square, color: Color) {
        let choices = self.layout_promotion_choices(to, color);
        self.promotion = Some(Promotion { from, to, color, choices });
    }

    /// Close the menu, returning the pending promotion if one was open.
    pub fn close_promotion(&mut self) -> Option<(Square, Square, Color)> {
        self.promotion.take().map(|p| (p.from, p.to, p.color))
    }

    pub fn promotion_open(&self) -> bool {
        self.promotion.is_some()
    }

    pub fn promotion_from(&self) -> Option<Square> {
        self.promotion.as_ref().map(|p| p.from)
    }

    /// The menu cells plus the backdrop quad behind them, for rendering.
    pub fn promotion_quads(&self, cursor: Option<(f32, f32)>) -> Vec<Rect> {
        let Some(p) = self.promotion.as_ref() else {
            return Vec::new();
        };
        let Some(first) = p.choices.first() else {
            return Vec::new();
        };
        let cell = first.rect.w;
        let total = PROMOTION_KINDS.len() as f32 * cell;
        let pad = cell * 0.12;

        let mut quads = Vec::with_capacity(p.choices.len() + 1);
        quads.push(Rect {
            x: first.rect.x - pad,
            y: first.rect.y - pad,
            w: total + pad * 2.0,
            h: cell + pad * 2.0,
            color: PANEL,
        });
        for c in &p.choices {
            let mut r = c.rect;
            if let Some((cx, cy)) = cursor {
                if contains(&r, cx, cy) {
                    r.color = SELECTED;
                }
            }
            quads.push(r);
        }
        quads
    }

    /// The pieces to draw inside the menu cells.
    pub fn promotion_choices(&self) -> &[PromotionChoice] {
        self.promotion.as_ref().map(|p| p.choices.as_slice()).unwrap_or(&[])
    }

    /// Which menu cell, if any, is under the cursor.
    pub fn promotion_at(&self, x: f32, y: f32) -> Option<Role> {
        let p = self.promotion.as_ref()?;
        p.choices.iter().find(|c| contains(&c.rect, x, y)).map(|c| c.role)
    }

    /// Clear any pending promotion (used when leaving play mode).
    pub fn clear_promotion(&mut self) {
        self.promotion = None;
    }

    fn apply_colors(&mut self) {
        for (i, rect) in self.squares.iter_mut().enumerate() {
            let row = i / 8;
            let col = i % 8;
            let mut base = if (row + col) % 2 == 0 { LIGHT } else { DARK };
            let sq = Self::square_at_index(i);
            // The blast sits directly on the board colour, below every
            // interaction highlight (which therefore never gets overridden).
            if self.explosion_flash > 0.0 && self.explosion.contains(&sq) {
                base = blend(base, EXPLOSION, self.explosion_flash);
            }
            rect.color = if self.checked_king == Some(sq) {
                CHECK
            } else if self.selected == Some(sq) {
                SELECTED
            } else if self.move_targets.contains(&sq) {
                TARGET
            } else if self
                .last_move
                .is_some_and(|(from, to)| from == sq || to == sq)
            {
                LAST_MOVE
            } else {
                base
            };
        }
    }

    fn square_at_index(i: usize) -> Square {
        let row = i / 8;
        let col = i % 8;
        Square::new(((7 - row) * 8 + col) as u32)
    }

    pub fn square_at(&self, x: f32, y: f32) -> Option<Square> {
        let bx = x - self.offset_x;
        let by = y - self.offset_y;
        if bx < 0.0 || by < 0.0 || bx >= self.board_px || by >= self.board_px {
            return None;
        }
        let col = (bx / self.sq_size) as usize;
        let row = (by / self.sq_size) as usize;
        if col >= 8 || row >= 8 {
            return None;
        }
        Some(Square::new(((7 - row) * 8 + col) as u32))
    }

    pub fn select(&mut self, sq: Square, targets: Vec<Square>) {
        self.selected = Some(sq);
        self.move_targets = targets;
        self.selected_pocket_piece = None;
        self.apply_colors();
    }

    /// Pick a pocket role for a drop. Selecting a pocket piece always clears
    /// the board selection, and vice versa.
    pub fn select_pocket(&mut self, role: Role, targets: Vec<Square>) {
        self.selected = None;
        self.move_targets = targets;
        self.selected_pocket_piece = Some(role);
        self.apply_colors();
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
        self.move_targets.clear();
        self.selected_pocket_piece = None;
        self.apply_colors();
    }

    pub fn selected_pocket_piece(&self) -> Option<Role> {
        self.selected_pocket_piece
    }

    /// Refresh the Atomic blast tint. `strength` is the already-eased flash
    /// alpha (0 hides it); the squares are only highlighted where no
    /// selection/target/check/last-move tint applies.
    pub fn set_explosion(&mut self, squares: &[Square], strength: f32) {
        self.explosion.clear();
        if strength > 0.0 {
            self.explosion.extend_from_slice(squares);
        }
        self.explosion_flash = strength.max(0.0);
        self.apply_colors();
    }

    /// Remove the blast tint (no-op when there is none, to avoid needless
    /// colour recomputation at idle).
    pub fn clear_explosion(&mut self) {
        if !self.explosion.is_empty() || self.explosion_flash != 0.0 {
            self.explosion.clear();
            self.explosion_flash = 0.0;
            self.apply_colors();
        }
    }

    /// Replace one colour's pocket row. `items` is grouped by role (pawns
    /// first); an empty list hides the row. Pocket rows sit on their own line
    /// outside the player bar, so names, clocks and the history column never
    /// collide with them.
    pub fn set_pocket(&mut self, color: Color, items: &[(Role, u8)], interactive: bool) {
        let sq = self.sq_size;
        let idx = match color {
            Color::Black => 0,
            Color::White => 1,
        };
        self.pocket_truncated[idx] = false;

        if sq <= 0.0 {
            self.pocket_end[idx] = self.offset_x;
            self.replace_pocket(color, Vec::new());
            return;
        }

        let icon = sq * 0.45;
        let gap = sq * 0.05;
        let badge_slot = icon * 0.8;
        let font_size = (sq * 0.35).max(14.0);
        // Above the top bar and below the bottom bar; both rows start at the
        // board's left edge.
        let cy = match color {
            Color::Black => self.offset_y - font_size * 1.1 - gap - icon * 0.5,
            Color::White => {
                self.offset_y + self.board_px + font_size * 1.3 + gap + icon * 0.5
            }
        };
        let limit = self.offset_x + self.board_px - sq * 0.05;

        let mut pieces = Vec::with_capacity(items.len());
        let mut x = self.offset_x;
        for &(role, count) in items {
            if count == 0 {
                continue;
            }
            if x + icon > limit {
                self.pocket_truncated[idx] = true;
                break;
            }
            pieces.push(PocketPiece {
                rect: Rect {
                    x,
                    y: cy - icon * 0.5,
                    w: icon,
                    h: icon,
                    color: PANEL_CELL,
                },
                role,
                color,
                count,
                interactive,
            });
            x += icon + gap;
            if count > 1 {
                x += badge_slot;
            }
        }
        let empty = pieces.is_empty();
        let end = if empty {
            self.offset_x
        } else if self.pocket_truncated[idx] {
            x
        } else {
            x - gap
        };
        self.replace_pocket(color, pieces);
        self.pocket_end[idx] = end;
    }

    fn replace_pocket(&mut self, color: Color, pieces: Vec<PocketPiece>) {
        match color {
            Color::Black => self.pocket_black = pieces,
            Color::White => self.pocket_white = pieces,
        }
    }

    pub fn pockets(&self, color: Color) -> &[PocketPiece] {
        match color {
            Color::Black => &self.pocket_black,
            Color::White => &self.pocket_white,
        }
    }

    pub fn pocket_truncated(&self, color: Color) -> bool {
        self.pocket_truncated[match color {
            Color::Black => 0,
            Color::White => 1,
        }]
    }

    /// X just past the last pocket element; the client puts an ellipsis here
    /// when the row was truncated.
    pub fn pocket_end(&self, color: Color) -> f32 {
        self.pocket_end[match color {
            Color::Black => 0,
            Color::White => 1,
        }]
    }

    /// Hit-test the interactive pocket pieces only: the opponent's pocket is
    /// display-only everywhere, and the non-moving side is inert in local play.
    pub fn pocket_at(&self, x: f32, y: f32) -> Option<(Color, Role)> {
        for color in [Color::Black, Color::White] {
            if let Some(p) = self
                .pockets(color)
                .iter()
                .find(|p| p.interactive && contains(&p.rect, x, y))
            {
                return Some((color, p.role));
            }
        }
        None
    }

    /// Background quads behind the selected pocket piece (drawn under the
    /// piece icons, like the promotion menu's hover tint).
    pub fn pocket_quads(&self) -> Vec<Rect> {
        let Some(role) = self.selected_pocket_piece else {
            return Vec::new();
        };
        let pad = self.sq_size * 0.06;
        self.pocket_black
            .iter()
            .chain(self.pocket_white.iter())
            .filter(|p| p.interactive && p.role == role)
            .map(|p| Rect {
                x: p.rect.x - pad,
                y: p.rect.y - pad,
                w: p.rect.w + pad * 2.0,
                h: p.rect.h + pad * 2.0,
                color: SELECTED,
            })
            .collect()
    }

    pub fn set_checked_king(&mut self, sq: Option<Square>) {
        self.checked_king = sq;
        self.apply_colors();
    }

    /// Mark the move that was just played; both squares get the muted-yellow
    /// tint until the next move, a mode switch, or a new game clears it.
    pub fn set_last_move(&mut self, mv: Option<(Square, Square)>) {
        self.last_move = mv;
        self.apply_colors();
    }

    pub fn last_move(&self) -> Option<(Square, Square)> {
        self.last_move
    }

    pub fn selected_square(&self) -> Option<Square> {
        self.selected
    }

    pub fn is_move_target(&self, sq: Square) -> bool {
        self.move_targets.contains(&sq)
    }

    /// Begin dragging the selected piece on `sq`; `(x, y)` is the press
    /// position, used as the click-vs-drag origin.
    pub fn start_drag(&mut self, sq: Square, x: f32, y: f32) {
        self.dragging = Some(sq);
        self.dragging_pocket = None;
        self.drag_origin = Some((x, y));
        self.drag_moved = false;
    }

    /// Begin dragging a pocket piece; mutually exclusive with a board drag.
    pub fn start_pocket_drag(&mut self, role: Role, x: f32, y: f32) {
        self.dragging = None;
        self.dragging_pocket = Some(role);
        self.drag_origin = Some((x, y));
        self.drag_moved = false;
    }

    /// Follow the cursor. Returns true once the cursor has moved beyond the
    /// click slop, i.e. this is a real drag rather than a click.
    pub fn drag_to(&mut self, x: f32, y: f32) -> bool {
        if let (Some((ox, oy)), true) = (self.drag_origin, self.is_dragging()) {
            let slop = (self.sq_size * 0.12).max(3.0);
            if (x - ox).abs() > slop || (y - oy).abs() > slop {
                self.drag_moved = true;
            }
        }
        self.drag_moved
    }

    /// Finish a drag, whether it played a move or was cancelled.
    pub fn end_drag(&mut self) {
        self.dragging = None;
        self.dragging_pocket = None;
        self.drag_origin = None;
        self.drag_moved = false;
    }

    /// True while either a board piece or a pocket piece is under the mouse.
    pub fn is_dragging(&self) -> bool {
        self.dragging.is_some() || self.dragging_pocket.is_some()
    }

    /// True once the press has turned into an actual drag; the renderer only
    /// hides the board piece at that point.
    pub fn drag_moved(&self) -> bool {
        self.drag_moved
    }

    pub fn dragging_square(&self) -> Option<Square> {
        self.dragging
    }

    pub fn dragging_pocket_piece(&self) -> Option<Role> {
        self.dragging_pocket
    }

    /// The piece under drag, looked up on the board being displayed.
    pub fn dragging_piece(&self, board: &VariantPosition) -> Option<(Square, Role, Color)> {
        let sq = self.dragging?;
        board
            .board()
            .piece_at(sq)
            .map(|piece| (sq, piece.role, piece.color))
    }

    pub fn board_offset(&self) -> (f32, f32) {
        (self.offset_x, self.offset_y)
    }

    pub fn square_size(&self) -> f32 {
        self.sq_size
    }
}

fn contains(r: &Rect, x: f32, y: f32) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h
}

#[cfg(test)]
mod tests {
    use super::*;
    use shakmaty::{File, Rank};

    fn ui_at(w: f32, h: f32) -> UI {
        let mut ui = UI::new();
        ui.layout(w, h);
        ui
    }

    #[test]
    fn menu_lists_every_promotion_choice() {
        let mut ui = ui_at(1000.0, 1000.0);
        assert!(!ui.promotion_open());
        assert!(ui.promotion_quads(None).is_empty());

        let from = Square::E7;
        let to = Square::E8;
        ui.open_promotion(from, to, Color::White);

        assert!(ui.promotion_open());
        assert_eq!(ui.promotion_from(), Some(from));

        let roles: Vec<Role> = ui.promotion_choices().iter().map(|c| c.role).collect();
        assert_eq!(
            roles,
            vec![Role::Queen, Role::Rook, Role::Bishop, Role::Knight]
        );
        for c in ui.promotion_choices() {
            assert_eq!(c.color, Color::White);
        }
    }

    #[test]
    fn white_promotion_menu_sits_above_the_board() {
        let mut ui = ui_at(1000.0, 1000.0);
        let to = Square::E8;
        ui.open_promotion(Square::E7, to, Color::White);

        let (_, oy) = ui.board_offset();
        for c in ui.promotion_choices() {
            assert!(c.rect.y + c.rect.h <= oy + 0.001, "menu overlaps the board");
        }
        // And it should be the topmost row of the menu area.
        assert!(ui.promotion_choices()[0].rect.y >= 0.0);
    }

    #[test]
    fn black_promotion_menu_sits_below_the_board() {
        let mut ui = ui_at(1000.0, 1000.0);
        let to = Square::E1;
        ui.open_promotion(Square::E2, to, Color::Black);

        let (_, oy) = ui.board_offset();
        let board_bottom = oy + ui.square_size() * 8.0;
        for c in ui.promotion_choices() {
            assert!(c.rect.y >= board_bottom - 0.001, "menu overlaps the board");
        }
        assert_eq!(ui.promotion_choices()[0].color, Color::Black);
    }

    #[test]
    fn menu_stays_on_screen_for_corner_files() {
        for file in 0..8u32 {
            for &(w, h) in &[(1000.0, 1000.0), (1600.0, 900.0), (900.0, 1600.0)] {
                let mut ui = ui_at(w, h);
                let to = Square::from_coords(File::new(file), Rank::Eighth);
                ui.open_promotion(
                    Square::from_coords(File::new(file), Rank::Seventh),
                    to,
                    Color::White,
                );
                for c in ui.promotion_choices() {
                    assert!(c.rect.x >= 0.0 && c.rect.x + c.rect.w <= w, "file {file} off-screen x");
                    assert!(c.rect.y >= 0.0 && c.rect.y + c.rect.h <= h, "file {file} off-screen y");
                }
            }
        }
    }

    #[test]
    fn hit_testing_maps_cells_to_roles() {
        let mut ui = ui_at(1000.0, 1000.0);
        let to = Square::C8;
        ui.open_promotion(Square::C7, to, Color::White);

        for c in ui.promotion_choices() {
            let cx = c.rect.x + c.rect.w / 2.0;
            let cy = c.rect.y + c.rect.h / 2.0;
            assert_eq!(ui.promotion_at(cx, cy), Some(c.role));
        }
        // A click well outside the menu picks nothing.
        assert_eq!(ui.promotion_at(1000.0 - 5.0, 1000.0 - 5.0), None);
    }

    #[test]
    fn hover_highlights_only_the_cell_under_the_cursor() {
        let mut ui = ui_at(1000.0, 1000.0);
        let to = Square::E8;
        ui.open_promotion(Square::E7, to, Color::White);

        let target = ui.promotion_choices()[2].rect; // bishop
        let quads = ui.promotion_quads(Some((target.x + 1.0, target.y + 1.0)));
        assert_eq!(quads.len(), 5, "backdrop + four cells");
        assert_eq!(quads[3].color, SELECTED);
        // The backdrop and the other cells keep their colours.
        assert_eq!(quads[0].color, PANEL);
        assert_eq!(quads[1].color, PANEL_CELL);
    }

    #[test]
    fn closing_the_menu_clears_it() {
        let mut ui = ui_at(1000.0, 1000.0);
        let from = Square::A7;
        let to = Square::A8;
        ui.open_promotion(from, to, Color::White);
        assert_eq!(ui.close_promotion(), Some((from, to, Color::White)));
        assert!(!ui.promotion_open());
        assert!(ui.promotion_quads(None).is_empty());
        assert!(ui.promotion_choices().is_empty());
        assert_eq!(ui.promotion_from(), None);
    }

    #[test]
    fn resize_relayouts_the_menu_without_overlapping_the_board() {
        let mut ui = ui_at(1000.0, 1000.0);
        let to = Square::E8;
        ui.open_promotion(Square::E7, to, Color::White);

        ui.layout(1400.0, 700.0);
        assert!(ui.promotion_open(), "resize should not drop a pending promotion");

        let (_, oy) = ui.board_offset();
        for c in ui.promotion_choices() {
            assert!(c.rect.y + c.rect.h <= oy + 0.001, "menu overlaps the board after resize");
            assert!(c.rect.x + c.rect.w <= 1400.0, "menu off-screen after resize");
        }
    }

    #[test]
    fn board_is_eighty_percent_of_the_window() {
        let ui = ui_at(1000.0, 1000.0);
        assert!(
            (ui.square_size() * 8.0 - 800.0).abs() < 0.001,
            "board should fill 80% of the smaller window dimension"
        );
    }

    #[test]
    fn drag_tracks_the_piece_and_the_cursor() {
        let mut ui = ui_at(1000.0, 1000.0);
        let board = VariantPosition::default();
        assert_eq!(ui.dragging_piece(&board), None);

        ui.select(Square::E2, vec![Square::E3, Square::E4]);
        ui.start_drag(Square::E2, 100.0, 100.0);
        assert!(ui.is_dragging());
        assert!(!ui.drag_moved(), "a fresh press is not a drag yet");
        assert_eq!(ui.dragging_square(), Some(Square::E2));
        assert_eq!(
            ui.dragging_piece(&board),
            Some((Square::E2, Role::Pawn, Color::White))
        );

        // A wobbly click stays inside the slop...
        assert!(!ui.drag_to(102.0, 101.0));
        // ...but leaving the square-size slop turns it into a drag.
        assert!(ui.drag_to(400.0, 100.0));
        // The selection survives, so the move targets stay highlighted.
        assert_eq!(ui.selected_square(), Some(Square::E2));
        assert!(ui.is_move_target(Square::E4));

        ui.end_drag();
        assert!(!ui.is_dragging());
        assert!(!ui.drag_moved());
        assert_eq!(ui.dragging_piece(&board), None);
        // Ending a drag never touches the selection; the client decides.
        assert_eq!(ui.selected_square(), Some(Square::E2));
    }

    fn square_color(ui: &UI, sq: Square) -> [f32; 3] {
        let idx = (7 - sq.rank().to_u32() as usize) * 8 + sq.file().to_u32() as usize;
        ui.squares[idx].color
    }

    #[test]
    fn last_move_tints_both_squares_under_the_other_highlights() {
        let mut ui = ui_at(1000.0, 1000.0);
        let (e2, e4) = (Square::E2, Square::E4);

        ui.set_last_move(Some((e2, e4)));
        assert_eq!(square_color(&ui, e2), LAST_MOVE);
        assert_eq!(square_color(&ui, e4), LAST_MOVE);
        // Untouched squares keep their base colour.
        assert_eq!(square_color(&ui, Square::A1), DARK);
        assert_eq!(square_color(&ui, Square::B1), LIGHT);

        // Selection and targets win over the last-move tint...
        ui.select(e2, vec![e4]);
        assert_eq!(square_color(&ui, e2), SELECTED);
        assert_eq!(square_color(&ui, e4), TARGET);
        // ...and so does a king in check.
        ui.set_checked_king(Some(e2));
        assert_eq!(square_color(&ui, e2), CHECK);

        // Clearing the higher-priority states uncovers the last-move tint.
        ui.set_checked_king(None);
        ui.clear_selection();
        assert_eq!(square_color(&ui, e2), LAST_MOVE);
        assert_eq!(square_color(&ui, e4), LAST_MOVE);

        ui.set_last_move(None);
        assert_eq!(square_color(&ui, e2), LIGHT);
        assert_eq!(square_color(&ui, e4), LIGHT);
    }

    #[test]
    fn pocket_rows_group_badges_and_hit_test_only_when_usable() {
        let mut ui = ui_at(1000.0, 1000.0);
        assert!(ui.pockets(Color::White).is_empty(), "no pockets by default");

        ui.set_pocket(Color::White, &[(Role::Pawn, 3), (Role::Knight, 1)], true);
        let pieces = ui.pockets(Color::White).to_vec();
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].role, Role::Pawn);
        assert_eq!(pieces[0].count, 3);
        assert_eq!(pieces[0].badge().as_deref(), Some("x3"));
        assert_eq!(pieces[1].badge(), None);
        assert_eq!(pieces[0].color, Color::White);
        assert_eq!(pieces[0].alpha(), 1.0);

        // Grouped entries share one row, starting at the board's left edge.
        let (ox, oy) = ui.board_offset();
        assert_eq!(pieces[0].rect.x, ox);
        assert_eq!(pieces[0].rect.y, pieces[1].rect.y);
        // White's row sits below the board, Black's above it.
        let board_bottom = oy + ui.square_size() * 8.0;
        assert!(pieces[0].rect.y >= board_bottom);
        ui.set_pocket(Color::Black, &[(Role::Queen, 1)], false);
        let black = ui.pockets(Color::Black)[0];
        assert!(black.rect.y + black.rect.h <= oy);

        // A display-only pocket is dimmed and inert; the usable one is solid
        // and clickable.
        assert_eq!(black.alpha(), 0.5);
        assert_eq!(ui.pocket_at(black.rect.x + 1.0, black.rect.y + 1.0), None);
        assert_eq!(
            ui.pocket_at(pieces[0].rect.x + 1.0, pieces[0].rect.y + 1.0),
            Some((Color::White, Role::Pawn))
        );
    }

    #[test]
    fn empty_pockets_render_nothing() {
        let mut ui = ui_at(1000.0, 1000.0);
        ui.set_pocket(Color::White, &[], true);
        assert!(ui.pockets(Color::White).is_empty());
        assert!(!ui.pocket_truncated(Color::White));
        assert_eq!(ui.pocket_end(Color::White), ui.board_offset().0);

        ui.set_pocket(Color::White, &[(Role::Pawn, 2)], true);
        assert_eq!(ui.pockets(Color::White).len(), 1);
        ui.set_pocket(Color::White, &[], true);
        assert!(
            ui.pockets(Color::White).is_empty(),
            "switching away from Crazyhouse clears the row"
        );
    }

    #[test]
    fn pocket_and_board_selection_are_mutually_exclusive() {
        let mut ui = ui_at(1000.0, 1000.0);
        ui.set_pocket(Color::White, &[(Role::Knight, 1)], true);

        ui.select(Square::E2, vec![Square::E4]);
        ui.select_pocket(Role::Knight, vec![Square::D4, Square::F6]);
        assert_eq!(ui.selected_square(), None, "pocket pick clears the board piece");
        assert_eq!(ui.selected_pocket_piece(), Some(Role::Knight));
        assert!(ui.is_move_target(Square::D4));
        assert_eq!(square_color(&ui, Square::D4), TARGET);
        assert!(!ui.pocket_quads().is_empty(), "the picked pocket piece is tinted");

        ui.select(Square::G1, vec![Square::F3]);
        assert_eq!(
            ui.selected_pocket_piece(),
            None,
            "board pick clears the pocket"
        );
        assert_eq!(ui.selected_square(), Some(Square::G1));
        assert!(ui.pocket_quads().is_empty());

        ui.select_pocket(Role::Knight, vec![Square::D4]);
        ui.clear_selection();
        assert_eq!(ui.selected_square(), None);
        assert_eq!(ui.selected_pocket_piece(), None);
        assert!(!ui.is_move_target(Square::D4));
    }

    #[test]
    fn pocket_drag_is_exclusive_with_a_board_drag() {
        let mut ui = ui_at(1000.0, 1000.0);
        ui.set_pocket(Color::White, &[(Role::Pawn, 1)], true);
        let pocket = ui.pockets(Color::White)[0];

        ui.start_pocket_drag(pocket.role, pocket.rect.x, pocket.rect.y);
        assert!(ui.is_dragging());
        assert_eq!(ui.dragging_pocket_piece(), Some(Role::Pawn));
        assert_eq!(ui.dragging_square(), None);
        assert!(!ui.drag_to(pocket.rect.x + 1.0, pocket.rect.y + 1.0));
        assert!(ui.drag_to(pocket.rect.x + 200.0, pocket.rect.y));

        ui.start_drag(Square::E2, 0.0, 0.0);
        assert_eq!(ui.dragging_pocket_piece(), None);
        assert_eq!(ui.dragging_square(), Some(Square::E2));

        ui.start_pocket_drag(Role::Pawn, 0.0, 0.0);
        assert_eq!(ui.dragging_square(), None);
        assert_eq!(ui.dragging_pocket_piece(), Some(Role::Pawn));

        ui.end_drag();
        assert!(!ui.is_dragging());
    }

    #[test]
    fn a_too_wide_pocket_row_is_truncated_at_the_board_edge() {
        let mut ui = ui_at(400.0, 300.0);
        let items: Vec<(Role, u8)> = (0..20).map(|_| (Role::Queen, 2)).collect();
        ui.set_pocket(Color::White, &items, true);

        assert!(ui.pocket_truncated(Color::White));
        assert!(ui.pockets(Color::White).len() < items.len());
        let right = ui.board_offset().0 + ui.square_size() * 8.0;
        assert!(ui.pocket_end(Color::White) <= right);
    }

    #[test]
    fn explosion_tint_sits_on_the_base_square_and_under_highlights() {
        let mut ui = ui_at(1000.0, 1000.0);
        let (e4, d5) = (Square::E4, Square::D5);

        // E4 is a light square: a full-strength blast turns it bright yellow.
        ui.set_explosion(&[e4], 1.0);
        assert_eq!(square_color(&ui, e4), EXPLOSION);
        // Half strength is a linear blend between the base and the blast.
        ui.set_explosion(&[e4], 0.5);
        let half = blend(LIGHT, EXPLOSION, 0.5);
        for (got, want) in square_color(&ui, e4).iter().zip(half.iter()) {
            assert!((got - want).abs() < 1e-6);
        }
        // A square outside the blast is untouched.
        ui.set_explosion(&[e4], 1.0);
        assert_eq!(square_color(&ui, d5), LIGHT);

        // Interaction highlights win over the blast...
        ui.set_last_move(Some((d5, Square::D6)));
        assert_eq!(square_color(&ui, d5), LAST_MOVE);
        ui.select(d5, vec![Square::C6]);
        assert_eq!(square_color(&ui, d5), SELECTED);
        ui.clear_selection();
        ui.set_last_move(None);
        assert_eq!(square_color(&ui, d5), LIGHT);

        // ...and clearing the blast restores the plain board colour.
        ui.clear_explosion();
        assert_eq!(square_color(&ui, e4), LIGHT);
    }
}
