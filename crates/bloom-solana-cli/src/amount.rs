//! Exact decimal parsing for SOL amounts.
//!
//! Accepts either integer lamports (`"5000"`) or fixed-point SOL with at
//! most nine decimals (`"1.5"`, `"0.000000001"`). Everything is integer
//! arithmetic: monetary values never pass through floating point. Leading
//! zeros are tolerated; negative values, more than nine decimals, empty
//! fractional parts (`"1."`), thousands separators, exponents, whitespace,
//! and lamport values above `u64::MAX` are all rejected.

use thiserror::Error;

/// Lamports per SOL.
pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
/// Maximum fractional digits (Solana's 9-decimal fixed point).
pub const MAX_DECIMALS: u32 = 9;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AmountError {
    #[error("amount is empty")]
    Empty,
    #[error("amount must not be negative: '{0}'")]
    Negative(String),
    #[error("amount has more than {MAX_DECIMALS} decimals: '{0}'")]
    TooManyDecimals(String),
    #[error("amount has an empty integer or fractional part: '{0}'")]
    EmptyPart(String),
    #[error("amount contains an invalid character {ch:?} in '{input}'")]
    BadCharacter { input: String, ch: char },
    #[error("amount '{0}' overflows 64-bit lamports")]
    Overflow(String),
}

/// Parse a user-supplied amount string into exact lamports.
///
/// The `unit` selects interpretation: `Sol` treats the integer part as whole
/// SOL and the fractional part as nine-decimal lamports; `Lamports` treats
/// the whole string as an integer lamport count and permits no decimal point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Lamports used by callers and tests
pub enum Unit {
    Sol,
    Lamports,
}

pub fn parse_amount(input: &str, unit: Unit) -> Result<u64, AmountError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AmountError::Empty);
    }
    if let Some(stripped) = trimmed.strip_prefix('-') {
        // Also catch '-0' style inputs uniformly.
        let _ = stripped;
        return Err(AmountError::Negative(trimmed.to_string()));
    }
    if trimmed.starts_with('+') {
        return Err(AmountError::BadCharacter {
            input: trimmed.to_string(),
            ch: '+',
        });
    }

    let (int_part, frac_part) = match trimmed.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (trimmed, None),
    };

    let frac_digits = match (frac_part, unit) {
        (None, _) => "",
        (Some(_), Unit::Lamports) => {
            return Err(AmountError::TooManyDecimals(trimmed.to_string()));
        }
        (Some(f), Unit::Sol) => f,
    };

    if int_part.is_empty() || (frac_part.is_some() && frac_digits.is_empty()) {
        return Err(AmountError::EmptyPart(trimmed.to_string()));
    }
    if frac_digits.len() > MAX_DECIMALS as usize {
        return Err(AmountError::TooManyDecimals(trimmed.to_string()));
    }
    for ch in int_part.chars().chain(frac_digits.chars()) {
        if !ch.is_ascii_digit() {
            return Err(AmountError::BadCharacter {
                input: trimmed.to_string(),
                ch,
            });
        }
    }

    // Integer part: whole SOL (or whole lamports in that unit).
    let mut whole: u128 = 0u128;
    for ch in int_part.chars() {
        whole = whole
            .checked_mul(10)
            .and_then(|w| w.checked_add(u128::from(ch as u8 - b'0')))
            .ok_or_else(|| AmountError::Overflow(trimmed.to_string()))?;
        if whole > u128::from(u64::MAX) {
            return Err(AmountError::Overflow(trimmed.to_string()));
        }
    }

    let scale = match unit {
        Unit::Sol => LAMPORTS_PER_SOL as u128,
        Unit::Lamports => 1,
    };
    let mut lamports = whole
        .checked_mul(scale)
        .ok_or_else(|| AmountError::Overflow(trimmed.to_string()))?;
    if lamports > u128::from(u64::MAX) {
        return Err(AmountError::Overflow(trimmed.to_string()));
    }

    // Fractional part: exactly nine-decimal lamports, zero-padded.
    for (idx, ch) in frac_digits.chars().enumerate() {
        let place = MAX_DECIMALS as usize - 1 - idx;
        let digit = u128::from(ch as u8 - b'0');
        let scaled = digit
            .checked_mul(10u128.checked_pow(place as u32).unwrap_or(u128::MAX))
            .ok_or_else(|| AmountError::Overflow(trimmed.to_string()))?;
        lamports = lamports
            .checked_add(scaled)
            .ok_or_else(|| AmountError::Overflow(trimmed.to_string()))?;
        if lamports > u128::from(u64::MAX) {
            return Err(AmountError::Overflow(trimmed.to_string()));
        }
    }

    Ok(lamports as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_integer_lamports() {
        assert_eq!(parse_amount("5000", Unit::Lamports).unwrap(), 5_000);
        assert_eq!(parse_amount("0", Unit::Lamports).unwrap(), 0);
        assert_eq!(
            parse_amount(&u64::MAX.to_string(), Unit::Lamports).unwrap(),
            u64::MAX
        );
    }

    #[test]
    fn parses_fixed_point_sol() {
        assert_eq!(parse_amount("1", Unit::Sol).unwrap(), LAMPORTS_PER_SOL);
        assert_eq!(parse_amount("1.5", Unit::Sol).unwrap(), 1_500_000_000);
        assert_eq!(parse_amount("0.000000001", Unit::Sol).unwrap(), 1);
        assert_eq!(
            parse_amount("1.000000001", Unit::Sol).unwrap(),
            1_000_000_001
        );
        assert_eq!(parse_amount("0.1", Unit::Sol).unwrap(), 100_000_000);
        assert_eq!(parse_amount(" 2.25 ", Unit::Sol).unwrap(), 2_250_000_000);
    }

    #[test]
    fn rejects_more_than_nine_decimals() {
        assert!(matches!(
            parse_amount("0.0000000001", Unit::Sol),
            Err(AmountError::TooManyDecimals(_))
        ));
        assert!(matches!(
            parse_amount("1.1234567890", Unit::Sol),
            Err(AmountError::TooManyDecimals(_))
        ));
        // Lamports unit never accepts a decimal point.
        assert!(matches!(
            parse_amount("1.5", Unit::Lamports),
            Err(AmountError::TooManyDecimals(_))
        ));
    }

    #[test]
    fn rejects_negative_plus_and_garbage() {
        for bad in [
            "-1", "-0.5", "+1", "1e9", "1_000", "1 000", "abc", "1.0.0", "", "   ",
        ] {
            assert!(
                parse_amount(bad, Unit::Sol).is_err(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_empty_parts() {
        assert!(matches!(
            parse_amount(".5", Unit::Sol),
            Err(AmountError::EmptyPart(_))
        ));
        assert!(matches!(
            parse_amount("1.", Unit::Sol),
            Err(AmountError::EmptyPart(_))
        ));
        assert!(matches!(
            parse_amount(".", Unit::Sol),
            Err(AmountError::EmptyPart(_))
        ));
    }

    #[test]
    fn rejects_overflow_beyond_u64() {
        // ~18446744073.7 SOL > u64 lamports.
        assert!(matches!(
            parse_amount("18446744074", Unit::Sol),
            Err(AmountError::Overflow(_))
        ));
        assert!(matches!(
            parse_amount("99999999999999999999999", Unit::Lamports),
            Err(AmountError::Overflow(_))
        ));
        // Exactly u64::MAX lamports is fine; one more is not.
        assert_eq!(
            parse_amount("18446744073709551615", Unit::Lamports).unwrap(),
            u64::MAX
        );
        assert!(matches!(
            parse_amount("18446744073709551616", Unit::Lamports),
            Err(AmountError::Overflow(_))
        ));
    }
}
