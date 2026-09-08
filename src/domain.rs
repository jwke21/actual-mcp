//! The vocabulary the tool layer speaks.
//!
//! Actual stores money as integer cents and dates as integers (`20260903`).
//! These newtypes carry those representations internally and convert **only**
//! at the serialization boundary, so FR-5.1 and FR-5.2 are enforced by the
//! type system rather than by remembering to convert at each call site.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Serialize, Serializer};

// ---------------------------------------------------------------- Money -----

/// An amount of money, held as integer cents.
///
/// Arithmetic stays in `i64`; the `f64` exists for exactly as long as it takes
/// to write one JSON number and is never read back in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Self = Self(0);

    pub const fn from_cents(c: i64) -> Self {
        Self(c)
    }

    pub const fn cents(self) -> i64 {
        self.0
    }

    pub fn is_outflow(self) -> bool {
        self.0 < 0
    }

    /// Magnitude, for reporting spend as a positive figure.
    pub const fn abs(self) -> Self {
        Self(self.0.abs())
    }
}

impl std::ops::Add for Money {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl std::ops::Sub for Money {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0)
    }
}

impl std::ops::Neg for Money {
    type Output = Self;
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl std::iter::Sum for Money {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, |acc, m| acc + m)
    }
}

impl<'a> std::iter::Sum<&'a Money> for Money {
    fn sum<I: Iterator<Item = &'a Money>>(iter: I) -> Self {
        iter.copied().sum()
    }
}

impl Serialize for Money {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // The one and only cents -> dollars crossing in the codebase.
        s.serialize_f64(self.0 as f64 / 100.0)
    }
}

impl JsonSchema for Money {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "Money".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "number",
            "description": "An amount in dollars. Negative values are outflows, positive are inflows."
        })
    }
}

// ----------------------------------------------------------- BudgetDate -----

/// A date as Actual stores it: `20260903`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BudgetDate(u32);

impl BudgetDate {
    /// A stand-in for a date the database should never contain.
    pub const EPOCH: Self = Self(19700101);

    /// Rejects impossible dates. Deliberately permissive about day-of-month:
    /// this guards against garbage, not against the 31st of February.
    pub fn from_int(n: u32) -> Option<Self> {
        let month = (n / 100) % 100;
        let day = n % 100;
        (n >= 1_000_101 && (1..=12).contains(&month) && (1..=31).contains(&day)).then_some(Self(n))
    }

    pub fn parse_iso(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let n: u32 = format!("{}{}{}", &s[0..4], &s[5..7], &s[8..10])
            .parse()
            .ok()?;
        Self::from_int(n)
    }

    pub const fn as_int(self) -> u32 {
        self.0
    }

    pub fn to_iso(self) -> String {
        format!(
            "{:04}-{:02}-{:02}",
            self.0 / 10_000,
            (self.0 / 100) % 100,
            self.0 % 100
        )
    }

    pub fn month(self) -> BudgetMonth {
        BudgetMonth(self.0 / 100)
    }
}

impl Serialize for BudgetDate {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_iso())
    }
}

impl JsonSchema for BudgetDate {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "BudgetDate".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "date",
            "description": "A date in ISO 8601 form, e.g. 2026-09-03."
        })
    }
}

// ---------------------------------------------------------- BudgetMonth -----

/// A budget month as Actual stores it: `202608`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BudgetMonth(u32);

impl BudgetMonth {
    pub fn from_int(n: u32) -> Option<Self> {
        let month = n % 100;
        (n >= 1_0001 && (1..=12).contains(&month)).then_some(Self(n))
    }

    /// Accepts `2026-08`.
    pub fn parse_iso(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() != 7 || b[4] != b'-' {
            return None;
        }
        let n: u32 = format!("{}{}", &s[0..4], &s[5..7]).parse().ok()?;
        Self::from_int(n)
    }

    pub const fn as_int(self) -> u32 {
        self.0
    }

    pub fn to_iso(self) -> String {
        format!("{:04}-{:02}", self.0 / 100, self.0 % 100)
    }

    /// The month after this one, rolling the year over at December.
    pub fn next(self) -> Self {
        if self.0 % 100 == 12 {
            Self((self.0 / 100 + 1) * 100 + 1)
        } else {
            Self(self.0 + 1)
        }
    }

    /// Inclusive bounds of this month, for date-range predicates.
    pub fn first_day(self) -> BudgetDate {
        BudgetDate(self.0 * 100 + 1)
    }

    pub fn last_day(self) -> BudgetDate {
        BudgetDate(self.0 * 100 + 31)
    }
}

impl Serialize for BudgetMonth {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_iso())
    }
}

impl JsonSchema for BudgetMonth {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "BudgetMonth".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A budget month in ISO form, e.g. 2026-08."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_serializes_as_dollars_not_cents() {
        assert_eq!(
            serde_json::to_string(&Money::from_cents(-94300)).unwrap(),
            "-943.0"
        );
        assert_eq!(
            serde_json::to_string(&Money::from_cents(5)).unwrap(),
            "0.05"
        );
        assert_eq!(serde_json::to_string(&Money::ZERO).unwrap(), "0.0");
    }

    /// Arithmetic must stay in integer cents; only serialization sees a float.
    #[test]
    fn money_sums_exactly() {
        let cents = [10, 20, 30].map(Money::from_cents);
        assert_eq!(cents.into_iter().sum::<Money>().cents(), 60);

        // 0.1 + 0.2 != 0.3 in f64, but these are integers all the way.
        let tricky = [Money::from_cents(10), Money::from_cents(20)];
        assert_eq!(tricky.iter().sum::<Money>().cents(), 30);
    }

    #[test]
    fn money_knows_direction() {
        assert!(Money::from_cents(-1).is_outflow());
        assert!(!Money::from_cents(0).is_outflow());
        assert_eq!(Money::from_cents(-943).abs().cents(), 943);
    }

    #[test]
    fn dates_round_trip() {
        let d = BudgetDate::from_int(20260903).unwrap();
        assert_eq!(d.to_iso(), "2026-09-03");
        assert_eq!(BudgetDate::parse_iso("2026-09-03"), Some(d));
        assert_eq!(serde_json::to_string(&d).unwrap(), "\"2026-09-03\"");
    }

    #[test]
    fn bad_dates_are_rejected() {
        assert_eq!(BudgetDate::parse_iso("2026-13-03"), None, "month 13");
        assert_eq!(BudgetDate::parse_iso("2026-09-32"), None, "day 32");
        assert_eq!(BudgetDate::parse_iso("2026-9-3"), None, "not zero padded");
        assert_eq!(BudgetDate::parse_iso("03/09/2026"), None, "not ISO");
        assert_eq!(BudgetDate::parse_iso(""), None);
        assert_eq!(BudgetDate::from_int(0), None);
    }

    #[test]
    fn months_round_trip_and_bound_days() {
        let m = BudgetMonth::parse_iso("2026-08").unwrap();
        assert_eq!(m.as_int(), 202608);
        assert_eq!(m.to_iso(), "2026-08");
        assert_eq!(m.first_day().as_int(), 20260801);
        assert_eq!(m.last_day().as_int(), 20260831);
        assert_eq!(BudgetMonth::parse_iso("2026-13"), None);
    }

    #[test]
    fn months_advance_across_a_year_boundary() {
        let dec = BudgetMonth::from_int(202612).unwrap();
        assert_eq!(dec.next(), BudgetMonth::from_int(202701).unwrap());
        let aug = BudgetMonth::from_int(202608).unwrap();
        assert_eq!(aug.next(), BudgetMonth::from_int(202609).unwrap());
    }

    #[test]
    fn a_date_knows_its_month() {
        assert_eq!(
            BudgetDate::from_int(20260903).unwrap().month(),
            BudgetMonth::from_int(202609).unwrap()
        );
    }
}
