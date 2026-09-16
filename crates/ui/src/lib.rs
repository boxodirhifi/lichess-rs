#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: [f32; 3],
}

pub struct UI {
    pub squares: Vec<Rect>,
}

impl UI {
    pub fn new() -> Self {
        Self {
            squares: Vec::with_capacity(64),
        }
    }

    pub fn layout(&mut self, screen_w: f32, screen_h: f32) {
        self.squares.clear();

        // Board takes up 80% of the smallest screen dimension
        let board_px = screen_w.min(screen_h) * 0.8;
        let sq_size = board_px / 8.0;
        let offset_x = (screen_w - board_px) / 2.0;
        let offset_y = (screen_h - board_px) / 2.0;

        // Classic Lichess colors
        let light = [240.0 / 255.0, 217.0 / 255.0, 181.0 / 255.0];
        let dark = [181.0 / 255.0, 136.0 / 255.0, 99.0 / 255.0];

        for r in 0..8 {
            for c in 0..8 {
                self.squares.push(Rect {
                    x: offset_x + c as f32 * sq_size,
                    y: offset_y + r as f32 * sq_size,
                    w: sq_size,
                    h: sq_size,
                    color: if (r + c) % 2 == 0 { light } else { dark },
                });
            }
        }
    }
}
