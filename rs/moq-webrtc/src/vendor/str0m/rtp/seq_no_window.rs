use super::SeqNo;

/// Tracks receipts in a fixed window of 128 sequence numbers without allocating.
///
/// The window covers the highest recorded sequence number and the preceding 127.
/// Unseen numbers within that window, including numbers before the initial receipt,
/// are accepted. Numbers ahead of the window are accepted; older numbers and duplicates
/// are rejected. [`Self::new_after`] additionally blocks all numbers at or below a
/// restored maximum.
///
/// Callers supply extended [`SeqNo`] values: this type does not extend wire sequence
/// numbers or calculate rollover counters. For authenticated traffic, check [`Self::accepts`]
/// before authentication and call [`Self::update`] only after authentication succeeds.
#[derive(Debug)]
pub(crate) struct SeqNoWindow {
    max: SeqNo,
    // Bit n records receipt of max - n.
    seen: u128,
}

impl SeqNoWindow {
    /// Starts a window with `seq` already recorded as received.
    pub fn new(seq: SeqNo) -> Self {
        Self { max: seq, seen: 1 }
    }

    /// Starts a window accepting only sequence numbers greater than `seq`.
    ///
    /// Use this when restoring a remembered maximum without its receipt bitmap.
    /// All positions at or below that maximum are blocked to prevent accepting replays
    /// after their individual receipt history has been discarded.
    pub fn new_after(seq: SeqNo) -> Self {
        Self {
            max: seq,
            seen: u128::MAX,
        }
    }

    /// Returns the highest recorded sequence number.
    pub fn max_seq(&self) -> SeqNo {
        self.max
    }

    /// Returns whether a packet with `seq` may be processed as a new receipt.
    ///
    /// - Above the highest recorded number: `true`, even if there is a gap.
    /// - Within the last 128 positions: `true` if that number has not been recorded
    ///   or blocked by [`Self::new_after`]; otherwise `false`.
    /// - Below those 128 positions: `false`. Receipt history has been discarded there,
    ///   so a delayed packet can no longer be distinguished from a replay.
    ///
    /// For example, a window created with `new(200.into())` accepts 201 and 199,
    /// rejects the recorded 200, and rejects 72 because it is too old.
    ///
    /// This is only a check: it does not mark `seq` as received or move the window.
    /// A `true` result also says nothing about the packet's authenticity. After the
    /// packet passes authentication, call [`Self::update`] to record its receipt.
    /// Subsequent checks for that sequence number will then return `false`.
    pub fn accepts(&self, seq: SeqNo) -> bool {
        seq > self.max || (*self.max - *seq < 128 && self.seen & (1u128 << (*self.max - *seq)) == 0)
    }

    /// Records a receipt, advancing the window when necessary.
    ///
    /// Duplicates and numbers older than the window leave it unchanged.
    pub fn update(&mut self, seq: SeqNo) {
        if seq > self.max {
            self.seen = self
                .seen
                .checked_shl((*seq - *self.max).min(128) as u32)
                .unwrap_or(0);
            self.max = seq;
        }
        if let Some(bit) = 1u128.checked_shl((*self.max - *seq).min(128) as u32) {
            self.seen |= bit;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_receipt_is_seen_but_earlier_numbers_are_not() {
        let window = SeqNoWindow::new(100.into());
        assert_eq!(window.max_seq(), 100.into());
        assert!(!window.accepts(100.into()));
        assert!(window.accepts(99.into()));
        assert!(window.accepts(0.into()));
        assert!(window.accepts(101.into()));

        let window = SeqNoWindow::new(0.into());
        assert!(!window.accepts(0.into()));
        assert!(window.accepts(1.into()));
    }

    #[test]
    fn reordered_receipts_are_recorded_without_moving_the_maximum() {
        let mut window = SeqNoWindow::new(65535.into());
        window.update(65537.into());
        assert!(window.accepts(65536.into()));
        // Checking eligibility must not record a packet before it is authenticated.
        assert!(window.accepts(65536.into()));
        window.update(65536.into());
        window.update(65536.into());
        assert_eq!(window.max_seq(), 65537.into());
        for seq in 65535..=65537 {
            assert!(!window.accepts(seq.into()));
        }
    }

    #[test]
    fn oldest_position_is_retained_until_it_leaves_the_window() {
        let mut window = SeqNoWindow::new(10.into());
        window.update(137.into());
        assert!(!window.accepts(10.into()));
        assert!(window.accepts(11.into()));
        window.update(138.into());
        assert!(!window.accepts(10.into()));
        assert!(window.accepts(11.into()));
        window.update(11.into());
        assert!(!window.accepts(11.into()));

        window.update(10.into());
        assert_eq!(window.max_seq(), 138.into());
        assert!(window.accepts(12.into()));
        assert!(!window.accepts(138.into()));
    }

    #[test]
    fn large_jumps_clear_the_previous_bitmap() {
        for gap in [128, 129, 1u64 << 32] {
            let mut window = SeqNoWindow::new(138.into());
            window.update(137.into());
            let next = 138 + gap;
            window.update(next.into());
            assert_eq!(window.max_seq(), next.into());
            assert!(!window.accepts(138.into()));
            assert!(!window.accepts(next.into()));
            assert!(window.accepts((next - 1).into()));
            assert!(window.accepts((next - 127).into()));
            assert!(!window.accepts((next - 128).into()));
        }
    }

    #[test]
    fn mixed_receipts_match_a_set_of_received_numbers() {
        use std::collections::BTreeSet;

        for start in [0u64, 65500, u64::MAX - 10000] {
            let mut window = SeqNoWindow::new(start.into());
            let mut received = BTreeSet::from([start]);
            let mut max = start;
            // Advance across word and window boundaries, fill gaps, repeat receipts,
            // and submit packets too old to retain. Near u64::MAX, advances saturate.
            let deltas = [1, 63, -1, 64, -64, 65, -127, -128, 127, 128, 129, 0];
            for delta in deltas.into_iter().cycle().take(240) {
                let seq = max.saturating_add_signed(delta);
                window.update(seq.into());
                received.insert(seq);
                max = max.max(seq);
                assert_eq!(window.max_seq(), max.into());

                // The reference keeps every receipt and applies only the age cutoff.
                for candidate in max.saturating_sub(130)..=max.saturating_add(2) {
                    let expected =
                        candidate >= max.saturating_sub(127) && !received.contains(&candidate);
                    assert_eq!(
                        window.accepts(candidate.into()),
                        expected,
                        "start={start}, receipt={seq}, max={max}, candidate={candidate}"
                    );
                }
            }
        }
    }

    #[test]
    fn restored_maximum_blocks_old_receipts_but_allows_new_gaps() {
        let mut window = SeqNoWindow::new_after(200.into());
        for seq in 0..=200 {
            assert!(!window.accepts(seq.into()));
        }
        assert!(window.accepts(201.into()));
        window.update(203.into());
        assert!(!window.accepts(200.into()));
        assert!(window.accepts(201.into()));
        assert!(window.accepts(202.into()));
        assert!(!window.accepts(203.into()));
        window.update(201.into());
        assert!(!window.accepts(201.into()));
        assert!(window.accepts(202.into()));
    }
}
