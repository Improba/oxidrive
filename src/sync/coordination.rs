//! Cross-device coordination helpers built on Drive `appProperties`.
//!
//! Version vectors track causal history as `device_id -> counter` pairs.

use std::collections::BTreeMap;

const APP_PROP_VERSION_VECTOR: &str = "ox_vv";
const APP_PROP_ORIGIN: &str = "ox_origin";

/// Three-way ordering for partial orders such as version-vector dominance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ordering3 {
    /// Both vectors are identical.
    Equal,
    /// Left side is greater-or-equal on every component and strictly greater on at least one.
    Dominates,
    /// Left side is lower-or-equal on every component and strictly lower on at least one.
    DominatedBy,
    /// Neither side dominates the other (concurrent edits).
    Concurrent,
}

/// Compact wrapper around a causal version vector (`device_id -> counter`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VersionVector {
    entries: BTreeMap<String, u64>,
}

impl VersionVector {
    /// Parses `device:count;device2:count2` (invalid segments are ignored).
    #[must_use]
    pub fn parse(input: &str) -> Self {
        let mut entries = BTreeMap::new();
        for segment in input.split(';') {
            let trimmed = segment.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some((device_raw, count_raw)) = trimmed.split_once(':') else {
                continue;
            };
            let device = device_raw.trim();
            if device.is_empty() {
                continue;
            }
            let Ok(count) = count_raw.trim().parse::<u64>() else {
                continue;
            };
            entries
                .entry(device.to_string())
                .and_modify(|existing: &mut u64| *existing = (*existing).max(count))
                .or_insert(count);
        }
        Self { entries }
    }

    /// Builds a vector from a persisted sync-record map.
    #[must_use]
    pub fn from_map(entries: &BTreeMap<String, u64>) -> Self {
        Self {
            entries: entries.clone(),
        }
    }

    /// Returns true when the vector has no usable component.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Consumes the wrapper and returns the underlying map.
    #[must_use]
    pub fn into_map(self) -> BTreeMap<String, u64> {
        self.entries
    }

    /// Parses the shared vector from Drive app properties (`ox_vv`).
    #[must_use]
    pub fn from_app_properties(props: &BTreeMap<String, String>) -> Self {
        props
            .get(APP_PROP_VERSION_VECTOR)
            .map_or_else(Self::default, |raw| Self::parse(raw))
    }

    /// Writes this vector back to Drive app properties.
    ///
    /// Always writes:
    /// - `ox_vv`: serialized version vector
    /// - `ox_origin`: device id that produced this write
    pub fn write_into_app_properties(
        &self,
        props: &mut BTreeMap<String, String>,
        origin_device: &str,
    ) {
        props.insert(APP_PROP_VERSION_VECTOR.to_string(), self.to_string());
        props.insert(APP_PROP_ORIGIN.to_string(), origin_device.to_string());
    }

    /// Increments the counter for `device`.
    pub fn increment(&mut self, device: &str) {
        if device.trim().is_empty() {
            return;
        }
        let counter = self.entries.entry(device.to_string()).or_insert(0);
        *counter = counter.saturating_add(1);
    }

    /// Returns the pointwise-maximum merge of two vectors.
    #[must_use]
    pub fn merge(&self, other: &VersionVector) -> VersionVector {
        let mut merged = self.entries.clone();
        for (device, counter) in &other.entries {
            merged
                .entry(device.clone())
                .and_modify(|existing: &mut u64| *existing = (*existing).max(*counter))
                .or_insert(*counter);
        }
        VersionVector { entries: merged }
    }

    /// Computes causal ordering between `self` and `other`.
    ///
    /// - `Dominates`: `self >= other` component-wise and strictly greater somewhere.
    /// - `DominatedBy`: symmetric case.
    /// - `Equal`: vectors are identical.
    /// - `Concurrent`: incomparable.
    #[must_use]
    pub fn dominance(&self, other: &VersionVector) -> Ordering3 {
        let mut self_ge_other = true;
        let mut self_le_other = true;
        let mut strictly_greater = false;
        let mut strictly_lower = false;

        for key in self.entries.keys().chain(other.entries.keys()) {
            let left = self.entries.get(key).copied().unwrap_or(0);
            let right = other.entries.get(key).copied().unwrap_or(0);
            if left < right {
                self_ge_other = false;
                strictly_lower = true;
            }
            if left > right {
                self_le_other = false;
                strictly_greater = true;
            }
        }

        if !strictly_greater && !strictly_lower {
            Ordering3::Equal
        } else if self_ge_other {
            Ordering3::Dominates
        } else if self_le_other {
            Ordering3::DominatedBy
        } else {
            Ordering3::Concurrent
        }
    }
}

impl std::fmt::Display for VersionVector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for (device, counter) in &self.entries {
            if !first {
                f.write_str(";")?;
            }
            first = false;
            write!(f, "{device}:{counter}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Ordering3, VersionVector};
    use std::collections::BTreeMap;

    #[test]
    fn parse_round_trip_is_stable() {
        let vv = VersionVector::parse("alice:1;bob:5");
        assert_eq!(vv.to_string(), "alice:1;bob:5");
        let back = VersionVector::parse(&vv.to_string());
        assert_eq!(back, vv);
    }

    #[test]
    fn parse_ignores_invalid_segments() {
        let vv = VersionVector::parse("alice:2;bad;:3;bob:x;carol:7;alice:4");
        assert_eq!(vv.to_string(), "alice:4;carol:7");
    }

    #[test]
    fn from_and_write_app_properties() {
        let mut props = BTreeMap::new();
        props.insert("ox_vv".to_string(), "dev-a:2".to_string());
        let vv = VersionVector::from_app_properties(&props);
        assert_eq!(vv.to_string(), "dev-a:2");

        let mut out = BTreeMap::new();
        vv.write_into_app_properties(&mut out, "dev-a");
        assert_eq!(out.get("ox_vv").map(String::as_str), Some("dev-a:2"));
        assert_eq!(out.get("ox_origin").map(String::as_str), Some("dev-a"));
    }

    #[test]
    fn increment_increases_counter() {
        let mut vv = VersionVector::default();
        vv.increment("dev-a");
        vv.increment("dev-a");
        vv.increment("dev-b");
        assert_eq!(vv.to_string(), "dev-a:2;dev-b:1");
    }

    #[test]
    fn merge_uses_componentwise_maximum() {
        let left = VersionVector::parse("a:1;b:3");
        let right = VersionVector::parse("a:4;c:2");
        let merged = left.merge(&right);
        assert_eq!(merged.to_string(), "a:4;b:3;c:2");
    }

    #[test]
    fn dominance_equal() {
        let left = VersionVector::parse("a:2;b:1");
        let right = VersionVector::parse("a:2;b:1");
        assert_eq!(left.dominance(&right), Ordering3::Equal);
    }

    #[test]
    fn dominance_dominates() {
        let left = VersionVector::parse("a:3;b:2");
        let right = VersionVector::parse("a:2;b:2");
        assert_eq!(left.dominance(&right), Ordering3::Dominates);
    }

    #[test]
    fn dominance_dominated_by() {
        let left = VersionVector::parse("a:1");
        let right = VersionVector::parse("a:1;b:1");
        assert_eq!(left.dominance(&right), Ordering3::DominatedBy);
    }

    #[test]
    fn dominance_concurrent() {
        let left = VersionVector::parse("a:2;b:1");
        let right = VersionVector::parse("a:1;b:2");
        assert_eq!(left.dominance(&right), Ordering3::Concurrent);
    }
}
