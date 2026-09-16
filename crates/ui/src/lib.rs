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

pub struct UI {
    pub squares: Vec<Rect>,
    pub selected: Option<(usize, usize)>,
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
            let r = i / 8;
            let c = i % 8;
            let base = if (r + c) % 2 == 0 { LIGHT } else { DARK };
            rect.color = if self.selected == Some((r, c)) { SELECTED } else { base };
        }
    }

    pub fn square_at(&self, x: f32, y: f32) -> Option<(usize, usize)> {
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
        Some((row, col))
    }

    pub fn handle_click(&mut self, x: f32, y: f32) {
        self.selected = match self.square_at(x, y) {
            Some(sq) if self.selected == Some(sq) => None,
            Some(sq) => Some(sq),
            None => None,
        };
        self.apply_colors();
    }

    pub fn board_offset(&self) -> (f32, f32) {
        (self.offset_x, self.offset_y)
    }

    pub fn square_size(&self) -> f32 {
        self.sq_size
    }
}
