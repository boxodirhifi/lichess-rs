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
const CHECK: [f32; 3] = [0.85, 0.25, 0.25];
const PANEL: [f32; 3] = [0.24, 0.23, 0.22];
const PANEL_CELL: [f32; 3] = [0.86, 0.82, 0.74];

/// One entry of the promotion menu: where it sits and what it promotes to.
#[derive(Debug, Clone, Copy)]
pub struct PromotionChoice {
    pub rect: Rect,
    pub role: Role,
    pub color: Color,
}

#[derive(Default)]
pub struct UI {
    pub squares: Vec<Rect>,
    selected: Option<Square>,
    move_targets: Vec<Square>,
    checked_king: Option<Square>,
    /// Square the piece being dragged came from, while the mouse is down.
    dragging: Option<Square>,
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
            let base = if (row + col) % 2 == 0 { LIGHT } else { DARK };
            let sq = Self::square_at_index(i);
            rect.color = if self.checked_king == Some(sq) {
                CHECK
            } else if self.selected == Some(sq) {
                SELECTED
            } else if self.move_targets.contains(&sq) {
                TARGET
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
        self.apply_colors();
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
        self.move_targets.clear();
        self.apply_colors();
    }

    pub fn set_checked_king(&mut self, sq: Option<Square>) {
        self.checked_king = sq;
        self.apply_colors();
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
        self.drag_origin = Some((x, y));
        self.drag_moved = false;
    }

    /// Follow the cursor. Returns true once the cursor has moved beyond the
    /// click slop, i.e. this is a real drag rather than a click.
    pub fn drag_to(&mut self, x: f32, y: f32) -> bool {
        if let (Some((ox, oy)), Some(_)) = (self.drag_origin, self.dragging) {
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
        self.drag_origin = None;
        self.drag_moved = false;
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// True once the press has turned into an actual drag; the renderer only
    /// hides the board piece at that point.
    pub fn drag_moved(&self) -> bool {
        self.drag_moved
    }

    pub fn dragging_square(&self) -> Option<Square> {
        self.dragging
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
}
