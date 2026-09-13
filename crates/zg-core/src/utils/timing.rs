//! Aggregated named timings.

use std::collections::BTreeMap;
use std::time::Instant;

use crate::types::TimingEntry;

/// Collects wall-clock durations aggregated by name.
#[derive(Debug, Default)]
pub struct TimingCollector {
    entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    total_ms: f64,
    count: u64,
}

impl TimingCollector {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one duration; empty names and non-finite values are discarded.
    /// Negative durations clamp to zero, counts clamp to one (mirroring the TS
    /// `Math.max` guards).
    pub fn add(&mut self, name: &str, duration_ms: f64, count: u64) {
        let name = name.trim();
        if name.is_empty() || !duration_ms.is_finite() {
            return;
        }
        let duration = duration_ms.max(0.0);
        let count = count.max(1);
        let entry = self.entries.entry(name.to_owned()).or_insert(Entry {
            total_ms: 0.0,
            count: 0,
        });
        entry.total_ms += duration;
        entry.count += count;
    }

    /// Times a closure and records it under `name` (records even on error).
    ///
    /// # Errors
    ///
    /// Returns the error produced by `task`, if any; timing itself never fails.
    pub fn time<T, E>(&mut self, name: &str, task: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let start = Instant::now();
        let result = task();
        self.add(name, start.elapsed().as_secs_f64() * 1000.0, 1);
        result
    }

    /// Rounded aggregate entries; `count` emitted only when > 1.
    #[must_use]
    pub fn entries(&self) -> Vec<TimingEntry> {
        self.entries
            .iter()
            .map(|(name, entry)| TimingEntry {
                name: name.clone(),
                duration_ms: entry.total_ms.round() as u64,
                count: (entry.count > 1).then_some(entry.count),
            })
            .collect()
    }
}

/// Tracks wall-clock across overlapping operations: records a single span from
/// the first start to the last stop.
#[derive(Debug)]
pub struct ConcurrentTiming<'a> {
    collector: &'a mut TimingCollector,
    name: String,
    active: usize,
    start: Instant,
}

impl<'a> ConcurrentTiming<'a> {
    pub fn start(collector: &'a mut TimingCollector, name: impl Into<String>) -> Self {
        Self {
            collector,
            name: name.into(),
            active: 1,
            start: Instant::now(),
        }
    }

    /// Marks one more overlapping operation beginning.
    pub fn enter(&mut self) {
        self.active += 1;
    }

    /// Marks one overlapping operation ending; records the span when the last
    /// one finishes.
    pub fn exit(&mut self) {
        self.active = self.active.saturating_sub(1);
        if self.active == 0 {
            let elapsed_ms = self.start.elapsed().as_secs_f64() * 1000.0;
            self.collector.add(&self.name, elapsed_ms, 1);
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_and_clamps() {
        let mut collector = TimingCollector::new();
        collector.add("scan", 10.4, 1);
        collector.add("scan", 4.6, 1);
        collector.add("  ", 1.0, 1);
        collector.add("neg", -1.0, 1);
        collector.add("bad", f64::NAN, 1);
        collector.add("zero", 1.0, 0);
        let by_name = |collector: &TimingCollector, name: &str| {
            collector
                .entries()
                .into_iter()
                .find(|e| e.name == name)
                .map(|e| (e.duration_ms, e.count))
        };
        assert_eq!(
            by_name(&collector, "scan"),
            Some((15, Some(2))),
            "scan aggregates both calls"
        );
        assert_eq!(
            by_name(&collector, "neg"),
            Some((0, None)),
            "neg clamps to 0"
        );
        assert_eq!(
            by_name(&collector, "zero"),
            Some((1, None)),
            "count clamps to 1"
        );
        assert_eq!(by_name(&collector, "bad"), None, "NaN discarded");
        assert_eq!(by_name(&collector, "  "), None, "empty name discarded");
    }

    #[test]
    fn count_emitted_when_multiple() {
        let mut collector = TimingCollector::new();
        collector.add("embed", 1.0, 3);
        let entries = collector.entries();
        assert_eq!(entries[0].count, Some(3));
    }

    #[test]
    fn concurrent_records_single_span() {
        let mut collector = TimingCollector::new();
        {
            let mut timing = ConcurrentTiming::start(&mut collector, "embed");
            timing.enter();
            timing.exit();
            timing.exit();
        }
        let entries = collector.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "embed");
    }

    #[test]
    fn time_records_on_error() {
        let mut collector = TimingCollector::new();
        let outcome: Result<(), ()> = collector.time("failing", || Err(()));
        assert!(outcome.is_err());
        assert_eq!(collector.entries().len(), 1);
    }
}
