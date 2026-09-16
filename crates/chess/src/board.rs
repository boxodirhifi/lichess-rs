use crate::{Color, Piece, PieceKind, Square};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    pub from: Square,
    pub to: Square,
    pub promotion: Option<PieceKind>,
}

const KNIGHT_DELTAS: [(i32, i32); 8] = [
    (1, 2), (2, 1), (2, -1), (1, -2),
    (-1, -2), (-2, -1), (-2, 1), (-1, 2),
];
const KING_DELTAS: [(i32, i32); 8] = [
    (1, 0), (1, 1), (0, 1), (-1, 1),
    (-1, 0), (-1, -1), (0, -1), (1, -1),
];
const BISHOP_DIRS: [(i32, i32); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];
const ROOK_DIRS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const QUEEN_DIRS: [(i32, i32); 8] = [
    (1, 0), (-1, 0), (0, 1), (0, -1),
    (1, 1), (1, -1), (-1, 1), (-1, -1),
];

#[derive(Debug, Clone)]
pub struct Board {
    squares: [Option<Piece>; 64],
}

impl Board {
    pub fn empty() -> Self {
        Board { squares: [None; 64] }
    }

    pub fn starting_position() -> Self {
        Self::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR")
            .expect("standard starting FEN must parse")
    }

    pub fn piece_at(&self, square: Square) -> Option<Piece> {
        self.squares[square.index()]
    }

    pub fn set_piece(&mut self, square: Square, piece: Option<Piece>) {
        self.squares[square.index()] = piece;
    }

    pub fn iter(&self) -> impl Iterator<Item = (Square, Piece)> + '_ {
        self.squares.iter().enumerate().filter_map(|(i, slot)| {
            slot.map(|piece| (Square::new(i as u8).unwrap(), piece))
        })
    }

    /// Pseudo-legal moves for the piece on `from` (empty if no piece there).
    pub fn moves_from(&self, from: Square) -> Vec<Move> {
        let piece = match self.piece_at(from) {
            Some(p) => p,
            None => return Vec::new(),
        };
        let mut moves = Vec::new();
        match piece.kind {
            PieceKind::Pawn => self.pawn_moves(from, piece.color, &mut moves),
            PieceKind::Knight => self.step_moves(from, piece.color, &KNIGHT_DELTAS, &mut moves),
            PieceKind::King => self.step_moves(from, piece.color, &KING_DELTAS, &mut moves),
            PieceKind::Bishop => self.slide_moves(from, piece.color, &BISHOP_DIRS, &mut moves),
            PieceKind::Rook => self.slide_moves(from, piece.color, &ROOK_DIRS, &mut moves),
            PieceKind::Queen => self.slide_moves(from, piece.color, &QUEEN_DIRS, &mut moves),
        }
        moves
    }

    pub fn make_move(&mut self, mv: Move) {
        let piece = match self.piece_at(mv.from) {
            Some(p) => p,
            None => return,
        };
        self.set_piece(mv.from, None);
        let placed = match mv.promotion {
            Some(kind) => Piece::new(kind, piece.color),
            None => piece,
        };
        self.set_piece(mv.to, Some(placed));
    }

    fn pawn_moves(&self, from: Square, color: Color, moves: &mut Vec<Move>) {
        let dir: i32 = match color {
            Color::White => 1,
            Color::Black => -1,
        };
        let start_rank: i32 = match color {
            Color::White => 1,
            Color::Black => 6,
        };
        let file = from.file() as i32;
        let rank = from.rank() as i32;

        // forward one
        let one = rank + dir;
        if (0..8).contains(&one) {
            let sq = Square::from_file_rank(file as u8, one as u8).unwrap();
            if self.piece_at(sq).is_none() {
                Self::push_pawn_move(from, sq, one, moves);
                // forward two from the starting rank
                if rank == start_rank {
                    let two = rank + 2 * dir;
                    let sq2 = Square::from_file_rank(file as u8, two as u8).unwrap();
                    if self.piece_at(sq2).is_none() {
                        moves.push(Move { from, to: sq2, promotion: None });
                    }
                }
            }
        }

        // diagonal captures
        for df in [-1i32, 1i32] {
            let nf = file + df;
            let nr = rank + dir;
            if (0..8).contains(&nf) && (0..8).contains(&nr) {
                let sq = Square::from_file_rank(nf as u8, nr as u8).unwrap();
                if let Some(target) = self.piece_at(sq) {
                    if target.color != color {
                        Self::push_pawn_move(from, sq, nr, moves);
                    }
                }
            }
        }
    }

    fn push_pawn_move(from: Square, to: Square, to_rank: i32, moves: &mut Vec<Move>) {
        if to_rank == 0 || to_rank == 7 {
            for promo in [PieceKind::Queen, PieceKind::Rook, PieceKind::Bishop, PieceKind::Knight] {
                moves.push(Move { from, to, promotion: Some(promo) });
            }
        } else {
            moves.push(Move { from, to, promotion: None });
        }
    }

    fn step_moves(&self, from: Square, color: Color, deltas: &[(i32, i32)], moves: &mut Vec<Move>) {
        let file = from.file() as i32;
        let rank = from.rank() as i32;
        for &(df, dr) in deltas {
            let nf = file + df;
            let nr = rank + dr;
            if !(0..8).contains(&nf) || !(0..8).contains(&nr) {
                continue;
            }
            let sq = Square::from_file_rank(nf as u8, nr as u8).unwrap();
            match self.piece_at(sq) {
                None => moves.push(Move { from, to: sq, promotion: None }),
                Some(p) if p.color != color => moves.push(Move { from, to: sq, promotion: None }),
                Some(_) => {}
            }
        }
    }

    fn slide_moves(&self, from: Square, color: Color, dirs: &[(i32, i32)], moves: &mut Vec<Move>) {
        let file = from.file() as i32;
        let rank = from.rank() as i32;
        for &(df, dr) in dirs {
            let mut nf = file + df;
            let mut nr = rank + dr;
            while (0..8).contains(&nf) && (0..8).contains(&nr) {
                let sq = Square::from_file_rank(nf as u8, nr as u8).unwrap();
                match self.piece_at(sq) {
                    None => {
                        moves.push(Move { from, to: sq, promotion: None });
                    }
                    Some(p) => {
                        if p.color != color {
                            moves.push(Move { from, to: sq, promotion: None });
                        }
                        break;
                    }
                }
                nf += df;
                nr += dr;
            }
        }
    }

    pub fn from_fen(fen: &str) -> Option<Self> {
        let placement = fen.split_whitespace().next()?;
        let mut board = Board::empty();
        let mut rank: i32 = 7;
        let mut file: i32 = 0;

        for ch in placement.chars() {
            match ch {
                '/' => {
                    rank -= 1;
                    file = 0;
                }
                '1'..='8' => {
                    file += ch.to_digit(10)? as i32;
                }
                _ => {
                    let color = if ch.is_uppercase() { Color::White } else { Color::Black };
                    let kind = match ch.to_ascii_lowercase() {
                        'p' => PieceKind::Pawn,
                        'n' => PieceKind::Knight,
                        'b' => PieceKind::Bishop,
                        'r' => PieceKind::Rook,
                        'q' => PieceKind::Queen,
                        'k' => PieceKind::King,
                        _ => return None,
                    };
                    if !(0..8).contains(&rank) || !(0..8).contains(&file) {
                        return None;
                    }
                    let sq = Square::from_file_rank(file as u8, rank as u8)?;
                    board.set_piece(sq, Some(Piece::new(kind, color)));
                    file += 1;
                }
            }
        }

        if rank != 0 {
            return None;
        }
        Some(board)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_position_has_32_pieces() {
        assert_eq!(Board::starting_position().iter().count(), 32);
    }

    #[test]
    fn pawn_starting_moves() {
        let board = Board::starting_position();
        let e2 = Square::from_file_rank(4, 1).unwrap();
        let dests: Vec<Square> = board.moves_from(e2).iter().map(|m| m.to).collect();
        assert_eq!(dests.len(), 2);
        assert!(dests.contains(&Square::from_file_rank(4, 2).unwrap())); // e3
        assert!(dests.contains(&Square::from_file_rank(4, 3).unwrap())); // e4
    }

    #[test]
    fn knight_starting_moves() {
        let board = Board::starting_position();
        let b1 = Square::from_file_rank(1, 0).unwrap();
        assert_eq!(board.moves_from(b1).len(), 2); // a3, c3
    }

    #[test]
    fn blocked_bishop_has_no_moves() {
        let board = Board::starting_position();
        let c1 = Square::from_file_rank(2, 0).unwrap();
        assert_eq!(board.moves_from(c1).len(), 0);
    }

    #[test]
    fn make_move_updates_board() {
        let mut board = Board::starting_position();
        let e2 = Square::from_file_rank(4, 1).unwrap();
        let e4 = Square::from_file_rank(4, 3).unwrap();
        board.make_move(Move { from: e2, to: e4, promotion: None });
        assert_eq!(board.piece_at(e2), None);
        assert_eq!(board.piece_at(e4), Some(Piece::new(PieceKind::Pawn, Color::White)));
        assert_eq!(board.iter().count(), 32);
    }
}
