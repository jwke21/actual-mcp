use std::time::{Duration, SystemTime};

/// What the server knows about how current its data is.
///
/// The two timestamps answer different questions (FR-3.2): `last_retrieval` is
/// when we last pulled from the Actual server, which we control; the newest
/// transaction date is the only available proxy for when transactions were last
/// imported from banks, which the user controls.
#[derive(Debug, Clone, PartialEq)]
pub struct Freshness {
    pub last_retrieval: Option<SystemTime>,
    /// `YYYYMMDD` as Actual stores it, e.g. 20260903.
    pub newest_transaction: Option<u32>,
}

/// Pure: `now` is a parameter, so no clock injection is needed to test this.
pub fn is_stale(last: Option<SystemTime>, now: SystemTime, ttl: Duration) -> bool {
    match last {
        None => true,
        // A last_retrieval in the future (clock skew) is not stale.
        Some(last) => now.duration_since(last).is_ok_and(|age| age >= ttl),
    }
}

pub fn age(last: Option<SystemTime>, now: SystemTime) -> Duration {
    last.and_then(|l| now.duration_since(l).ok())
        .unwrap_or_default()
}

/// Actual stores dates as integers: 20260903 -> "2026-09-03".
pub fn iso_date(yyyymmdd: u32) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        yyyymmdd / 10_000,
        (yyyymmdd / 100) % 100,
        yyyymmdd % 100
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(60);

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn never_retrieved_is_stale() {
        assert!(is_stale(None, at(1_000), TTL));
    }

    #[test]
    fn within_ttl_is_fresh() {
        assert!(!is_stale(Some(at(1_000)), at(1_059), TTL));
    }

    #[test]
    fn at_ttl_is_stale() {
        assert!(is_stale(Some(at(1_000)), at(1_060), TTL));
    }

    /// Clock skew must not make the data look infinitely stale.
    #[test]
    fn future_retrieval_is_not_stale() {
        assert!(!is_stale(Some(at(2_000)), at(1_000), TTL));
        assert_eq!(age(Some(at(2_000)), at(1_000)), Duration::ZERO);
    }

    #[test]
    fn formats_actual_dates() {
        assert_eq!(iso_date(20260903), "2026-09-03");
        assert_eq!(iso_date(20260101), "2026-01-01");
        assert_eq!(iso_date(19991231), "1999-12-31");
    }
}
