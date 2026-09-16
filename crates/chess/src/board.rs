use crate::{Color, Piece, PieceKind, Square};

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

    /// Iterate over all occupied squares as (square, piece).
    pub fn iter(&self) -> impl Iterator<Item = (Square, Piece)> + '_ {
        self.squares.iter().enumerate().filter_map(|(i, slot)| {
            slot.map(|piece| (Square::new(i as u8).unwrap(), piece))
        })
    }

    /// Parse the piece-placement portion of a FEN string.
    pub fn from_fen(fen: &str) -> Option<Self> {
        let placement = fen.split_whitespace().next()?;
        let mut board = Board::empty();
        let mut rank: i32 = 7; // FEN begins at rank 8
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
                    let color = if ch.is_uppercase() {
                        Color::White
                    } else {
                        Color::Black
                    };
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

        // A valid placement must fill all 8 ranks.
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
        let board = Board::starting_position();
        assert_eq!(board.iter().count(), 32);
    }

    #[test]
    fn starting_position_key_pieces() {
        let board = Board::starting_position();

        let e2 = Square::from_file_rank(4, 1).unwrap();
        assert_eq!(
            board.piece_at(e2),
            Some(Piece::new(PieceKind::Pawn, Color::White))
        );

        let e1 = Square::from_file_rank(4, 0).unwrap();
        assert_eq!(
            board.piece_at(e1),
            Some(Piece::new(PieceKind::King, Color::White))
        );

        let e8 = Square::from_file_rank(4, 7).unwrap();
        assert_eq!(
            board.piece_at(e8),
            Some(Piece::new(PieceKind::King, Color::Black))
        );

        let d8 = Square::from_file_rank(3, 7).unwrap();
        assert_eq!(
            board.piece_at(d8),
            Some(Piece::new(PieceKind::Queen, Color::Black))
        );
    }

    #[test]
    fn empty_squares_are_none() {
        let board = Board::starting_position();
        let e4 = Square::from_file_rank(4, 3).unwrap();
        assert_eq!(board.piece_at(e4), None);
    }

    #[test]
    fn invalid_fen_rejected() {
        assert!(Board::from_fen("not a fen").is_none());
        assert!(Board::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP").is_none()); // missing rank
    }
}
