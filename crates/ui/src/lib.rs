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

pub struct UI {
    pub squares: Vec<Rect>,
    selected: Option<chess::Square>,
    move_targets: Vec<chess::Square>,
    checked_king: Option<chess::Square>,
    board_px: f32,
    sq_size: f32,
    offset_x: f32,
    offset_y: f32,
}

impl UI {
    pub fn new() -> Self {
        Self {
            squares: Vec::with_capacity(64),
            selected: None,
            move_targets: Vec::new(),
            checked_king: None,
            board_px: 0.0,
            sq_size: 0.0,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }

    pub fn layout(&mut self, screen_w: f32, screen_h: f32) {
        self.squares.clear();
        let board_px = screen_w.min(screen_h) * 0.8;
        let sq_size = board_px / 8.0;
        let offset_x = (screen_w - board_px) / 2.0;
        let offset_y = (screen_h - board_px) / 2.0;

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

    fn square_at_index(i: usize) -> chess::Square {
        let row = i / 8;
        let col = i % 8;
        chess::Square::from_file_rank(col as u8, (7 - row) as u8).unwrap()
    }

    pub fn square_at(&self, x: f32, y: f32) -> Option<chess::Square> {
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
        chess::Square::from_file_rank(col as u8, (7 - row) as u8)
    }

    pub fn select(&mut self, sq: chess::Square, targets: Vec<chess::Square>) {
        self.selected = Some(sq);
        self.move_targets = targets;
        self.apply_colors();
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
        self.move_targets.clear();
        self.apply_colors();
    }

    pub fn set_checked_king(&mut self, sq: Option<chess::Square>) {
        self.checked_king = sq;
        self.apply_colors();
    }

    pub fn selected_square(&self) -> Option<chess::Square> {
        self.selected
    }

    pub fn is_move_target(&self, sq: chess::Square) -> bool {
        self.move_targets.contains(&sq)
    }

    pub fn board_offset(&self) -> (f32, f32) {
        (self.offset_x, self.offset_y)
    }

    pub fn square_size(&self) -> f32 {
        self.sq_size
    }
}
