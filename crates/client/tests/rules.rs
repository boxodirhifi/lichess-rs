use shakmaty::fen::Fen;
use shakmaty::san::San;
use shakmaty::{perft, CastlingMode, CastlingSide, Chess, Color, File, Move, Position, Square};

fn position(fen: &str) -> Chess {
    Fen::from_ascii(fen.as_bytes())
        .expect("valid FEN")
        .into_position::<Chess>(CastlingMode::Standard)
        .expect("legal position")
}

#[test]
fn pinned_piece_cannot_leave_pin_line() {
    let pos = position("4r2k/8/8/8/4R3/8/8/4K3 w - - 0 1");
    let legal: Vec<Move> = pos
        .legal_moves()
        .into_iter()
        .filter(|mv| mv.from() == Some(Square::E4))
        .collect();

    assert_eq!(legal.len(), 6);
    for mv in legal {
        assert_eq!(mv.to().file(), File::E);
    }
}

#[test]
fn en_passant_cannot_expose_king() {
    // b5xc6 en passant would remove both b5 and c5, exposing king a5 to rook h5.
    let pos = position("4k3/8/8/KPp4r/8/8/8/8 w - c6 0 1");
    let legal = pos.legal_moves();

    assert!(!legal
        .iter()
        .any(|mv| mv.from() == Some(Square::B5) && mv.to() == Square::C6));
}

#[test]
fn cannot_castle_through_check() {
    let pos = position("1k3r2/8/8/8/8/8/8/R3K2R w KQ - 0 1");
    let legal = pos.legal_moves();

    assert!(!legal.iter().any(|mv| {
        mv.from() == Some(Square::E1) && mv.castling_side() == Some(CastlingSide::KingSide)
    }));
    assert!(legal.iter().any(|mv| {
        mv.from() == Some(Square::E1) && mv.castling_side() == Some(CastlingSide::QueenSide)
    }));
}

#[test]
fn replay_real_lichess_game_san() {
    // A live game pulled from Lichess TV (Sicilian, includes disambiguated
    // moves like Nbd7/Rac1/Nce2, castling, and the capture cxb5).
    let sans = "e4 c5 Nf3 e6 d4 cxd4 Nxd4 a6 Bd3 Nf6 O-O Qc7 Qe2 d6 c4 \
                Nbd7 Nc3 Ne5 Bg5 Be7 Rac1 h6 Bh4 Nxd3 Qxd3 O-O Kh1 Bd7 \
                f4 Rfd8 b3 Be8 Rce1 Rac8 Nce2 b5 cxb5";
    let mut pos = Chess::default();

    for (i, token) in sans.split_whitespace().enumerate() {
        let san = San::from_ascii(token.as_bytes())
            .unwrap_or_else(|e| panic!("failed to parse '{token}' at ply {i}: {e}"));
        let mv = san
            .to_move(&pos)
            .unwrap_or_else(|e| panic!("illegal '{token}' at ply {i}: {e}"));
        pos.play_unchecked(mv);
    }

    // Captures happened, so fewer than 32 pieces remain.
    assert!(pos.board().occupied().count() < 32);
    // White just played cxb5, so it's black's turn.
    assert_eq!(pos.turn(), Color::Black);
}

#[test]
fn perft_start_position_depth_four() {
    assert_eq!(perft(&Chess::default(), 4), 197_281);
}
