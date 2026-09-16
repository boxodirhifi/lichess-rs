use std::fmt;

/// A board square, indexed 0..63 where index = rank * 8 + file.
/// `a1` = 0, `h1` = 7, `a8` = 56, `h8` = 63.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Square(u8);

impl Square {
    pub const fn new(index: u8) -> Option<Self> {
        if index < 64 {
            Some(Square(index))
        } else {
            None
        }
    }

    pub const fn from_file_rank(file: u8, rank: u8) -> Option<Self> {
        if file < 8 && rank < 8 {
            Some(Square(rank * 8 + file))
        } else {
            None
        }
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// File 0..7 mapping to 'a'..'h'.
    pub const fn file(self) -> u8 {
        self.0 % 8
    }

    /// Rank 0..7 mapping to 1..8.
    pub const fn rank(self) -> u8 {
        self.0 / 8
    }
}

impl fmt::Display for Square {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let file = (b'a' + self.file()) as char;
        write!(f, "{}{}", file, self.rank() + 1)
    }
}

impl fmt::Debug for Square {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Square({})", self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_names() {
        assert_eq!(Square::from_file_rank(0, 0).unwrap().to_string(), "a1");
        assert_eq!(Square::from_file_rank(4, 3).unwrap().to_string(), "e4");
        assert_eq!(Square::from_file_rank(7, 7).unwrap().to_string(), "h8");
    }

    #[test]
    fn out_of_bounds() {
        assert!(Square::new(64).is_none());
        assert!(Square::from_file_rank(8, 0).is_none());
        assert!(Square::from_file_rank(0, 8).is_none());
    }
}
