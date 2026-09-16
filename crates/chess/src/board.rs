use crate::{Color, Piece, PieceKind, Square};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    pub from: Square,
    pub to: Square,
    pub promotion: Option<PieceKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CastlingRights {
    pub white_kingside: bool,
    pub white_queenside: bool,
    pub black_kingside: bool,
    pub black_queenside: bool,
}

impl CastlingRights {
    pub fn all() -> Self {
        CastlingRights {
            white_kingside: true,
            white_queenside: true,
            black_kingside: true,
            black_queenside: true,
        }
    }
    pub fn none() -> Self {
        CastlingRights {
            white_kingside: false,
            white_queenside: false,
            black_kingside: false,
            black_queenside: false,
        }
    }
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
    castling: CastlingRights,
    en_passant: Option<Square>,
}

impl Board {
    pub fn empty() -> Self {
        Board {
            squares: [None; 64],
            castling: CastlingRights::none(),
            en_passant: None,
        }
    }

    pub fn starting_position() -> Self {
        Self::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1")
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

    /// Pseudo-legal moves for the piece on `from`.
    pub fn moves_from(&self, from: Square) -> Vec<Move> {
        let piece = match self.piece_at(from) {
            Some(p) => p,
            None => return Vec::new(),
        };
        let mut moves = Vec::new();
        match piece.kind {
            PieceKind::Pawn => self.pawn_moves(from, piece.color, &mut moves),
            PieceKind::Knight => self.step_moves(from, piece.color, &KNIGHT_DELTAS, &mut moves),
            PieceKind::King => {
                self.step_moves(from, piece.color, &KING_DELTAS, &mut moves);
                self.castling_moves(from, piece.color, &mut moves);
            }
            PieceKind::Bishop => self.slide_moves(from, piece.color, &BISHOP_DIRS, &mut moves),
            PieceKind::Rook => self.slide_moves(from, piece.color, &ROOK_DIRS, &mut moves),
            PieceKind::Queen => self.slide_moves(from, piece.color, &QUEEN_DIRS, &mut moves),
        }
        moves
    }

    /// Fully legal moves — excludes anything that leaves our king in check.
    pub fn legal_moves_from(&self, from: Square) -> Vec<Move> {
        let piece = match self.piece_at(from) {
            Some(p) => p,
            None => return Vec::new(),
        };
        self.moves_from(from)
            .into_iter()
            .filter(|mv| {
                let mut copy = self.clone();
                copy.make_move(*mv);
                !copy.in_check(piece.color)
            })
            .collect()
    }

    pub fn make_move(&mut self, mv: Move) {
        let piece = match self.piece_at(mv.from) {
            Some(p) => p,
            None => return,
        };

        let is_pawn = piece.kind == PieceKind::Pawn;
        let is_en_passant = is_pawn
            && self.en_passant == Some(mv.to)
            && mv.from.file() != mv.to.file()
            && self.piece_at(mv.to).is_none();
        let is_castle = piece.kind == PieceKind::King
            && (mv.to.file() as i32 - mv.from.file() as i32).abs() == 2;

        // En passant removes the pawn that double-pushed (it sits beside the target).
        if is_en_passant {
            let captured = Square::from_file_rank(mv.to.file(), mv.from.rank()).unwrap();
            self.set_piece(captured, None);
        }

        self.set_piece(mv.from, None);
        let placed = match mv.promotion {
            Some(kind) => Piece::new(kind, piece.color),
            None => piece,
        };
        self.set_piece(mv.to, Some(placed));

        // Castling also moves the rook.
        if is_castle {
            let rank = mv.from.rank();
            let (rook_from, rook_to) = if mv.to.file() > mv.from.file() {
                // kingside: h -> f
                (
                    Square::from_file_rank(7, rank).unwrap(),
                    Square::from_file_rank(5, rank).unwrap(),
                )
            } else {
                // queenside: a -> d
                (
                    Square::from_file_rank(0, rank).unwrap(),
                    Square::from_file_rank(3, rank).unwrap(),
                )
            };
            if let Some(rook) = self.piece_at(rook_from) {
                self.set_piece(rook_from, None);
                self.set_piece(rook_to, Some(rook));
            }
        }

        // Reset en passant target, then set it if this was a double push.
        self.en_passant = None;
        if is_pawn && (mv.to.rank() as i32 - mv.from.rank() as i32).abs() == 2 {
            let ep_rank = (mv.from.rank() as i32 + mv.to.rank() as i32) / 2;
            self.en_passant = Square::from_file_rank(mv.from.file(), ep_rank as u8);
        }

        // Update castling rights.
        if piece.kind == PieceKind::King {
            match piece.color {
                Color::White => {
                    self.castling.white_kingside = false;
                    self.castling.white_queenside = false;
                }
                Color::Black => {
                    self.castling.black_kingside = false;
                    self.castling.black_queenside = false;
                }
            }
        }
        self.revoke_rights_for_corner(mv.from);
        self.revoke_rights_for_corner(mv.to);
    }

    fn revoke_rights_for_corner(&mut self, sq: Square) {
        match (sq.file(), sq.rank()) {
            (0, 0) => self.castling.white_queenside = false,
            (7, 0) => self.castling.white_kingside = false,
            (0, 7) => self.castling.black_queenside = false,
            (7, 7) => self.castling.black_kingside = false,
            _ => {}
        }
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

        let one = rank + dir;
        if (0..8).contains(&one) {
            let sq = Square::from_file_rank(file as u8, one as u8).unwrap();
            if self.piece_at(sq).is_none() {
                Self::push_pawn_move(from, sq, one, moves);
                if rank == start_rank {
                    let two = rank + 2 * dir;
                    let sq2 = Square::from_file_rank(file as u8, two as u8).unwrap();
                    if self.piece_at(sq2).is_none() {
                        moves.push(Move { from, to: sq2, promotion: None });
                    }
                }
            }
        }

        // Diagonal captures
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

        // En passant capture
        if let Some(ep) = self.en_passant {
            let ep_rank = ep.rank() as i32;
            let ep_file = ep.file() as i32;
            if ep_rank == rank + dir {
                for df in [-1i32, 1i32] {
                    if file + df == ep_file {
                        moves.push(Move { from, to: ep, promotion: None });
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
                    None => moves.push(Move { from, to: sq, promotion: None }),
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

    fn castling_moves(&self, from: Square, color: Color, moves: &mut Vec<Move>) {
        // Can't castle out of check.
        if self.is_square_attacked(from, color.opposite()) {
            return;
        }
        if from.file() != 4 {
            return;
        }
        let rank = from.rank();
        let (kingside, queenside) = match color {
            Color::White => (self.castling.white_kingside, self.castling.white_queenside),
            Color::Black => (self.castling.black_kingside, self.castling.black_queenside),
        };

        // Kingside: f and g empty; f and g not attacked.
        if kingside {
            let f = Square::from_file_rank(5, rank).unwrap();
            let g = Square::from_file_rank(6, rank).unwrap();
            let h = Square::from_file_rank(7, rank).unwrap();
            let rook_ok = matches!(
                self.piece_at(h),
                Some(Piece { kind: PieceKind::Rook, color }) if color == color
            );
            if rook_ok && self.piece_at(f).is_none() && self.piece_at(g).is_none()
                && !self.is_square_attacked(f, color.opposite())
                && !self.is_square_attacked(g, color.opposite())
            {
                moves.push(Move { from, to: g, promotion: None });
            }
        }

        // Queenside: b, c, d empty; d and c not attacked.
        if queenside {
            let b = Square::from_file_rank(1, rank).unwrap();
            let c = Square::from_file_rank(2, rank).unwrap();
            let d = Square::from_file_rank(3, rank).unwrap();
            let a = Square::from_file_rank(0, rank).unwrap();
            let rook_ok = matches!(
                self.piece_at(a),
                Some(Piece { kind: PieceKind::Rook, color }) if color == color
            );
            if rook_ok && self.piece_at(b).is_none() && self.piece_at(c).is_none() && self.piece_at(d).is_none()
                && !self.is_square_attacked(d, color.opposite())
                && !self.is_square_attacked(c, color.opposite())
            {
                moves.push(Move { from, to: c, promotion: None });
            }
        }
    }

    pub fn in_check(&self, color: Color) -> bool {
        let king = match self.king_square(color) {
            Some(sq) => sq,
            None => return false,
        };
        self.is_square_attacked(king, color.opposite())
    }

    pub fn king_square(&self, color: Color) -> Option<Square> {
        self.iter().find_map(|(sq, piece)| {
            if piece.kind == PieceKind::King && piece.color == color {
                Some(sq)
            } else {
                None
            }
        })
    }

    pub fn is_square_attacked(&self, square: Square, by: Color) -> bool {
        let file = square.file() as i32;
        let rank = square.rank() as i32;

        let pawn_dir: i32 = match by {
            Color::White => 1,
            Color::Black => -1,
        };
        for df in [-1i32, 1i32] {
            let pf = file + df;
            let pr = rank - pawn_dir;
            if (0..8).contains(&pf) && (0..8).contains(&pr) {
                let sq = Square::from_file_rank(pf as u8, pr as u8).unwrap();
                if let Some(p) = self.piece_at(sq) {
                    if p.color == by && p.kind == PieceKind::Pawn {
                        return true;
                    }
                }
            }
        }

        for &(df, dr) in KNIGHT_DELTAS.iter() {
            let nf = file + df;
            let nr = rank + dr;
            if (0..8).contains(&nf) && (0..8).contains(&nr) {
                let sq = Square::from_file_rank(nf as u8, nr as u8).unwrap();
                if let Some(p) = self.piece_at(sq) {
                    if p.color == by && p.kind == PieceKind::Knight {
                        return true;
                    }
                }
            }
        }

        for &(df, dr) in KING_DELTAS.iter() {
            let nf = file + df;
            let nr = rank + dr;
            if (0..8).contains(&nf) && (0..8).contains(&nr) {
                let sq = Square::from_file_rank(nf as u8, nr as u8).unwrap();
                if let Some(p) = self.piece_at(sq) {
                    if p.color == by && p.kind == PieceKind::King {
                        return true;
                    }
                }
            }
        }

        for &(df, dr) in ROOK_DIRS.iter() {
            if self.slide_attacks(square, by, df, dr, &[PieceKind::Rook, PieceKind::Queen]) {
                return true;
            }
        }
        for &(df, dr) in BISHOP_DIRS.iter() {
            if self.slide_attacks(square, by, df, dr, &[PieceKind::Bishop, PieceKind::Queen]) {
                return true;
            }
        }

        false
    }

    fn slide_attacks(&self, from: Square, by: Color, df: i32, dr: i32, kinds: &[PieceKind]) -> bool {
        let mut nf = from.file() as i32 + df;
        let mut nr = from.rank() as i32 + dr;
        while (0..8).contains(&nf) && (0..8).contains(&nr) {
            let sq = Square::from_file_rank(nf as u8, nr as u8).unwrap();
            if let Some(p) = self.piece_at(sq) {
                return p.color == by && kinds.contains(&p.kind);
            }
            nf += df;
            nr += dr;
        }
        false
    }

    pub fn from_fen(fen: &str) -> Option<Self> {
        let fields: Vec<&str> = fen.split_whitespace().collect();
        let placement = fields.first()?;

        let mut squares = [None; 64];
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
                    squares[sq.index()] = Some(Piece::new(kind, color));
                    file += 1;
                }
            }
        }
        if rank != 0 {
            return None;
        }

        let mut castling = CastlingRights::none();
        if fields.len() > 2 {
            for ch in fields[2].chars() {
                match ch {
                    'K' => castling.white_kingside = true,
                    'Q' => castling.white_queenside = true,
                    'k' => castling.black_kingside = true,
                    'q' => castling.black_queenside = true,
                    _ => {}
                }
            }
        }

        let en_passant = if fields.len() > 3 && fields[3] != "-" {
            Self::parse_square(fields[3])
        } else {
            None
        };

        Some(Board { squares, castling, en_passant })
    }

    fn parse_square(s: &str) -> Option<Square> {
        let mut chars = s.chars();
        let file_ch = chars.next()?;
        let rank_ch = chars.next()?;
        let file = (file_ch as u8).checked_sub(b'a')?;
        let rank = (rank_ch as u8).checked_sub(b'1')?;
        if file >= 8 || rank >= 8 {
            return None;
        }
        Square::from_file_rank(file, rank)
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
    }

    #[test]
    fn knight_starting_moves() {
        let board = Board::starting_position();
        let b1 = Square::from_file_rank(1, 0).unwrap();
        assert_eq!(board.moves_from(b1).len(), 2);
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
    }

    #[test]
    fn pinned_piece_cannot_leave_pin_line() {
        let board = Board::from_fen("4r2k/8/8/8/4R3/8/8/4K3").unwrap();
        let e4 = Square::from_file_rank(4, 3).unwrap();
        let legal = board.legal_moves_from(e4);
        assert_eq!(legal.len(), 6);
        for mv in &legal {
            assert_eq!(mv.to.file(), 4);
        }
    }

    #[test]
    fn detects_check() {
        let board = Board::from_fen("4r2k/8/8/8/8/8/8/4K3").unwrap();
        assert!(board.in_check(Color::White));
        assert!(!board.in_check(Color::Black));
    }

    #[test]
    fn king_cannot_move_into_check() {
        let board = Board::from_fen("4r2k/8/8/8/8/8/8/4K3").unwrap();
        let e1 = Square::from_file_rank(4, 0).unwrap();
        let legal = board.legal_moves_from(e1);
        assert_eq!(legal.len(), 4);
        for mv in &legal {
            assert_ne!(mv.to.file(), 4);
        }
    }

    #[test]
    fn castling_available() {
        let board = Board::from_fen("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1").unwrap();
        let e1 = Square::from_file_rank(4, 0).unwrap();
        let dests: Vec<Square> = board.legal_moves_from(e1).iter().map(|m| m.to).collect();
        assert!(dests.contains(&Square::from_file_rank(6, 0).unwrap())); // g1
        assert!(dests.contains(&Square::from_file_rank(2, 0).unwrap())); // c1
    }

    #[test]
    fn cannot_castle_through_check() {
        let board = Board::from_fen("5r1k/8/8/8/8/8/8/R3K2R w KQ - 0 1").unwrap();
        let e1 = Square::from_file_rank(4, 0).unwrap();
        let dests: Vec<Square> = board.legal_moves_from(e1).iter().map(|m| m.to).collect();
        assert!(!dests.contains(&Square::from_file_rank(6, 0).unwrap())); // g1 blocked (f1 attacked)
        assert!(dests.contains(&Square::from_file_rank(2, 0).unwrap())); // c1 still ok
    }

    #[test]
    fn castling_moves_rook() {
        let mut board = Board::from_fen("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1").unwrap();
        let e1 = Square::from_file_rank(4, 0).unwrap();
        let g1 = Square::from_file_rank(6, 0).unwrap();
        let f1 = Square::from_file_rank(5, 0).unwrap();
        let h1 = Square::from_file_rank(7, 0).unwrap();
        board.make_move(Move { from: e1, to: g1, promotion: None });
        assert_eq!(board.piece_at(g1).unwrap().kind, PieceKind::King);
        assert_eq!(board.piece_at(f1).unwrap().kind, PieceKind::Rook);
        assert!(board.piece_at(h1).is_none());
    }

    #[test]
    fn en_passant_capture() {
        let board = Board::from_fen("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1").unwrap();
        let e5 = Square::from_file_rank(4, 4).unwrap();
        let dests: Vec<Square> = board.legal_moves_from(e5).iter().map(|m| m.to).collect();
        assert!(dests.contains(&Square::from_file_rank(3, 5).unwrap())); // d6
    }

    #[test]
    fn en_passant_removes_captured_pawn() {
        let mut board = Board::from_fen("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1").unwrap();
        let e5 = Square::from_file_rank(4, 4).unwrap();
        let d5 = Square::from_file_rank(3, 4).unwrap();
        let d6 = Square::from_file_rank(3, 5).unwrap();
        let mv = board.legal_moves_from(e5).into_iter().find(|m| m.to == d6).unwrap();
        board.make_move(mv);
        assert!(board.piece_at(d5).is_none());
        assert!(board.piece_at(d6).is_some());
        assert!(board.piece_at(e5).is_none());
    }

    #[test]
    fn en_passant_cannot_expose_king() {
        // b5xc6 en passant would remove both b5 and c5, exposing king a5 to rook h5.
        let board = Board::from_fen("4k3/8/8/KPp4r/8/8/8/8 w - c6 0 1").unwrap();
        let b5 = Square::from_file_rank(1, 4).unwrap();
        let c6 = Square::from_file_rank(2, 5).unwrap();
        let dests: Vec<Square> = board.legal_moves_from(b5).iter().map(|m| m.to).collect();
        assert!(!dests.contains(&c6));
    }
}
