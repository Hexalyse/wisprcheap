//! Hybrid logical clock (SPEC.md section 4): timestamps that follow the wall clock but never go
//! backwards and always move past every timestamp seen, so last-writer-wins works despite clock skew.

use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hlc {
    pub ms: u64,
    pub counter: u16,
    pub node: String,
}

impl Hlc {
    /// `<ms, 13 digits>-<counter, 4 hex>-<node>`: string order equals time order.
    pub fn parse(text: &str) -> Option<Hlc> {
        let mut parts = text.splitn(3, '-');
        let ms = parts.next()?;
        let counter = parts.next()?;
        let node = parts.next()?;
        if ms.len() != 13 || !ms.bytes().all(|b| b.is_ascii_digit()) || counter.len() != 4 || node.is_empty() {
            return None;
        }
        // Device ids are base64url: letters, digits, `_` and `-`.
        if !node.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return None;
        }
        Some(Hlc { ms: ms.parse().ok()?, counter: u16::from_str_radix(counter, 16).ok()?, node: node.to_string() })
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:013}-{:04x}-{}", self.ms, self.counter, self.node)
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.ms, self.counter, &self.node).cmp(&(other.ms, other.counter, &other.node))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A device's clock. Persist `last()` and restore it with [`Clock::observe`] across restarts.
#[derive(Debug, Clone)]
pub struct Clock {
    node: String,
    ms: u64,
    counter: u16,
}

impl Clock {
    pub fn new(node: impl Into<String>) -> Self {
        Self { node: node.into(), ms: 0, counter: 0 }
    }

    /// A new timestamp, greater than every timestamp issued or observed so far.
    pub fn now(&mut self, wall_ms: u64) -> Hlc {
        if wall_ms > self.ms {
            self.ms = wall_ms;
            self.counter = 0;
        } else if self.counter == u16::MAX {
            self.ms += 1;
            self.counter = 0;
        } else {
            self.counter += 1;
        }
        Hlc { ms: self.ms, counter: self.counter, node: self.node.clone() }
    }

    /// Merges a timestamp received from elsewhere.
    pub fn observe(&mut self, other: &Hlc) {
        if other.ms > self.ms || (other.ms == self.ms && other.counter > self.counter) {
            self.ms = other.ms;
            self.counter = other.counter;
        }
    }

    pub fn last(&self) -> Hlc {
        Hlc { ms: self.ms, counter: self.counter, node: self.node.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_parse_and_order() {
        let a = Hlc { ms: 1_759_154_400_123, counter: 0, node: "dev_a".into() };
        assert_eq!(a.to_string(), "1759154400123-0000-dev_a");
        assert_eq!(Hlc::parse(&a.to_string()), Some(a.clone()));
        let b = Hlc { ms: 1_759_154_400_123, counter: 10, node: "dev_a".into() };
        assert!(a < b && a.to_string() < b.to_string());
        let c = Hlc { ms: 1_759_154_400_123, counter: 10, node: "dev_b".into() };
        assert!(b < c && b.to_string() < c.to_string());
        assert!(Hlc::parse("123-0000-dev").is_none());
        assert!(Hlc::parse("1759154400123-0000-").is_none());
        assert!(Hlc::parse("1759154400123-0000-dev a").is_none());
        let dashed = Hlc::parse("1759154400123-0000-dev_a-b_C").unwrap();
        assert_eq!(dashed.node, "dev_a-b_C");
    }

    #[test]
    fn clock_is_monotonic_and_moves_past_observed() {
        let mut c = Clock::new("dev_a");
        let t1 = c.now(1000);
        let t2 = c.now(900); // wall clock went back
        assert!(t2 > t1);
        c.observe(&Hlc { ms: 5000, counter: 3, node: "dev_b".into() });
        let t3 = c.now(1000);
        assert!(t3.ms == 5000 && t3.counter == 4);
        let t4 = c.now(6000);
        assert_eq!((t4.ms, t4.counter), (6000, 0));
    }
}
