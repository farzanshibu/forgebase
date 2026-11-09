//! Money arithmetic in integer minor units (cents, paise, yen): currency
//! codes, tiered and volume pricing, proration, tax and discounts. Nothing
//! here uses floating point; every division rounds half away from zero.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::{BillingError, Result};

/// Largest amount of one line or invoice (10^13 minor units).
pub const MAX_AMOUNT: i64 = 10_000_000_000_000;

/// An amount in minor units of an ISO 4217 currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Money {
    /// Minor units (cents for USD, paise for INR, yen for JPY).
    pub amount: i64,
    /// ISO 4217 code, upper case.
    pub currency: String,
}

impl Money {
    /// An amount of `currency`.
    pub fn new(amount: i64, currency: impl Into<String>) -> Self {
        Self {
            amount,
            currency: currency.into(),
        }
    }
}

/// Validates and normalises an ISO 4217 code (three ASCII letters, upper
/// case on return).
pub fn currency(code: &str) -> Result<String> {
    let c = code.trim();
    if c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()) {
        Ok(c.to_ascii_uppercase())
    } else {
        Err(BillingError::invalid(
            "invalid_currency",
            format!("{code:?} is not an ISO 4217 currency code"),
        ))
    }
}

/// Number of minor-unit digits of a currency (ISO 4217 exponent).
pub fn exponent(currency: &str) -> u32 {
    match currency {
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX" | "UYI" | "VND"
        | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
        _ => 2,
    }
}

/// `a * num / den` rounded half away from zero, without overflow.
pub fn mul_div(a: i64, num: i64, den: i64) -> i64 {
    if den == 0 {
        return 0;
    }
    let (a, num, den) = (i128::from(a), i128::from(num), i128::from(den));
    let p = a * num;
    let neg = (p < 0) != (den < 0);
    let (p, d) = (p.abs(), den.abs());
    let q = (p + d / 2) / d;
    let q = if neg { -q } else { q };
    i64::try_from(q).unwrap_or(if neg { i64::MIN } else { i64::MAX })
}

/// Checked multiplication of an amount, refusing results beyond
/// [`MAX_AMOUNT`].
pub fn checked_amount(a: i64, b: i64) -> Result<i64> {
    a.checked_mul(b)
        .filter(|v| v.abs() <= MAX_AMOUNT)
        .ok_or_else(|| BillingError::invalid("amount_too_large", "amount exceeds the supported range"))
}

/// One pricing tier: units up to `up_to` (inclusive; `None`: no upper
/// bound) cost `unit_amount` each plus `flat_amount` once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Tier {
    /// Last unit of the tier (`null`: the rest).
    #[serde(default)]
    pub up_to: Option<i64>,
    /// Price per unit, minor units.
    #[serde(default)]
    pub unit_amount: i64,
    /// Fixed fee when the tier is reached, minor units.
    #[serde(default)]
    pub flat_amount: i64,
}

/// How quantity becomes an amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    /// `unit_amount × quantity`.
    PerUnit,
    /// Each unit at its own tier's price (plus each reached tier's flat fee).
    Graduated,
    /// Every unit at the price of the tier the total falls in.
    Volume,
}

impl Scheme {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::PerUnit => "per_unit",
            Scheme::Graduated => "graduated",
            Scheme::Volume => "volume",
        }
    }

    /// Parses a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "per_unit" => Scheme::PerUnit,
            "graduated" => Scheme::Graduated,
            "volume" => Scheme::Volume,
            _ => return None,
        })
    }
}

/// Validates tiers: ascending `up_to`, the last one unbounded, amounts not
/// negative.
pub fn check_tiers(tiers: &[Tier]) -> Result<()> {
    let bad = |m: &str| Err(BillingError::invalid("invalid_tiers", m.to_owned()));
    if tiers.is_empty() || tiers.len() > 50 {
        return bad("tiered prices need 1 to 50 tiers");
    }
    let mut prev = 0;
    for (i, t) in tiers.iter().enumerate() {
        if t.unit_amount < 0 || t.flat_amount < 0 || t.unit_amount > MAX_AMOUNT || t.flat_amount > MAX_AMOUNT {
            return bad("tier amounts must be between 0 and 10^13");
        }
        match t.up_to {
            Some(u) if u <= prev => return bad("up_to must increase from tier to tier"),
            Some(u) => prev = u,
            None if i + 1 != tiers.len() => return bad("only the last tier may be unbounded"),
            None => {}
        }
    }
    if tiers.last().is_some_and(|t| t.up_to.is_some()) {
        return bad("the last tier must be unbounded (up_to: null)");
    }
    Ok(())
}

/// The amount of `quantity` units.
pub fn price_amount(scheme: Scheme, unit_amount: i64, tiers: &[Tier], quantity: i64) -> Result<i64> {
    if quantity <= 0 {
        return Ok(0);
    }
    match scheme {
        Scheme::PerUnit => checked_amount(unit_amount, quantity),
        Scheme::Volume => {
            let tier = tiers
                .iter()
                .find(|t| t.up_to.is_none_or(|u| quantity <= u))
                .ok_or_else(|| BillingError::Internal("tiers without an unbounded tier".into()))?;
            checked_amount(tier.unit_amount, quantity)?
                .checked_add(tier.flat_amount)
                .ok_or_else(|| BillingError::invalid("amount_too_large", "amount exceeds the supported range"))
        }
        Scheme::Graduated => {
            let mut total: i64 = 0;
            let mut lower = 0;
            for t in tiers {
                if quantity <= lower {
                    break;
                }
                let upper = t.up_to.unwrap_or(i64::MAX).min(quantity);
                let units = upper - lower;
                let add = checked_amount(t.unit_amount, units)?
                    .checked_add(t.flat_amount)
                    .ok_or_else(|| BillingError::invalid("amount_too_large", "amount exceeds the supported range"))?;
                total = total
                    .checked_add(add)
                    .filter(|v| *v <= MAX_AMOUNT)
                    .ok_or_else(|| BillingError::invalid("amount_too_large", "amount exceeds the supported range"))?;
                lower = upper;
            }
            Ok(total)
        }
    }
}

/// The part of `amount` for the time left in a period: `amount × (end −
/// at) / (end − start)`, rounded. `at` is clamped into the period.
pub fn prorate(amount: i64, start: i64, end: i64, at: i64) -> i64 {
    if end <= start {
        return 0;
    }
    let at = at.clamp(start, end);
    mul_div(amount, end - at, end - start)
}

/// Tax of a line of `amount` at `rate_bp` basis points: exclusive tax is
/// added on top; inclusive tax is the part of `amount` that is tax.
pub fn tax(amount: i64, rate_bp: i64, inclusive: bool) -> i64 {
    if amount <= 0 || rate_bp <= 0 {
        return 0;
    }
    if inclusive {
        amount - mul_div(amount, 10_000, 10_000 + rate_bp)
    } else {
        mul_div(amount, rate_bp, 10_000)
    }
}

/// Spreads `total` over `weights` in proportion, so the parts add up to
/// `total` exactly (the largest weight takes the rounding remainder).
pub fn allocate(total: i64, weights: &[i64]) -> Vec<i64> {
    let sum: i64 = weights.iter().copied().filter(|w| *w > 0).sum();
    if sum <= 0 || total == 0 {
        return vec![0; weights.len()];
    }
    let mut out: Vec<i64> = weights
        .iter()
        .map(|w| if *w > 0 { mul_div(total, *w, sum) } else { 0 })
        .collect();
    let diff = total - out.iter().sum::<i64>();
    if diff != 0
        && let Some((i, _)) = weights.iter().enumerate().max_by_key(|(_, w)| **w)
    {
        out[i] += diff;
    }
    out
}

/// Lower-case hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0xf)]));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(up_to: Option<i64>, unit: i64, flat: i64) -> Tier {
        Tier {
            up_to,
            unit_amount: unit,
            flat_amount: flat,
        }
    }

    #[test]
    fn rounding_is_half_away_from_zero() {
        assert_eq!(mul_div(5, 1, 2), 3);
        assert_eq!(mul_div(-5, 1, 2), -3);
        assert_eq!(mul_div(4, 1, 3), 1);
        assert_eq!(mul_div(i64::MAX, 2, 2), i64::MAX);
    }

    #[test]
    fn currencies() {
        assert_eq!(currency(" usd").unwrap(), "USD");
        assert!(currency("US").is_err());
        assert!(currency("U$D").is_err());
        assert_eq!(exponent("JPY"), 0);
        assert_eq!(exponent("KWD"), 3);
        assert_eq!(exponent("INR"), 2);
    }

    #[test]
    fn graduated_and_volume_tiers() {
        let tiers = vec![t(Some(1000), 0, 0), t(Some(5000), 2, 0), t(None, 1, 500)];
        check_tiers(&tiers).unwrap();
        // 1000 free, 4000 × 2, 1000 × 1 + 500 flat.
        assert_eq!(price_amount(Scheme::Graduated, 0, &tiers, 6000).unwrap(), 8000 + 1000 + 500);
        assert_eq!(price_amount(Scheme::Graduated, 0, &tiers, 800).unwrap(), 0);
        // Volume: all 6000 at the third tier.
        assert_eq!(price_amount(Scheme::Volume, 0, &tiers, 6000).unwrap(), 6000 + 500);
        assert_eq!(price_amount(Scheme::Volume, 0, &tiers, 3000).unwrap(), 6000);
        assert_eq!(price_amount(Scheme::PerUnit, 250, &[], 4).unwrap(), 1000);
        assert!(check_tiers(&[t(Some(10), 1, 0)]).is_err());
        assert!(check_tiers(&[t(Some(10), 1, 0), t(Some(5), 1, 0), t(None, 1, 0)]).is_err());
        assert!(price_amount(Scheme::PerUnit, MAX_AMOUNT, &[], 2).is_err());
    }

    #[test]
    fn proration_tax_and_allocation() {
        // Half of a 30-day period left.
        assert_eq!(prorate(3000, 0, 30, 15), 1500);
        assert_eq!(prorate(1000, 0, 3, 1), 667);
        assert_eq!(prorate(1000, 0, 10, 20), 0);
        assert_eq!(tax(10_000, 1800, false), 1800);
        // 118.00 including 18 % holds 18.00 of tax.
        assert_eq!(tax(11_800, 1800, true), 1800);
        assert_eq!(tax(-100, 1800, false), 0);
        let parts = allocate(100, &[1, 1, 1]);
        assert_eq!(parts.iter().sum::<i64>(), 100);
        assert_eq!(allocate(50, &[0, 10]), vec![0, 50]);
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
    }
}
