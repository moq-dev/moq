/// Sliding replay window for DTLS record sequence numbers.
///
/// Maintains the latest accepted sequence number and a 64-bit bitmap of the
/// last 64 seen sequence numbers to reject duplicates and old records.
///
/// Each epoch should have its own `ReplayWindow` instance. The caller is
/// responsible for routing records to the correct per-epoch window.
#[derive(Debug, Default)]
pub struct ReplayWindow {
    max_seq: u64,
    window: u64,
}

impl ReplayWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if the given sequence number is acceptable (not a replay, not too old).
    /// Read-only: does not modify the window state.
    pub fn check(&self, seqno: u64) -> bool {
        if seqno > self.max_seq {
            true
        } else {
            let offset = self.max_seq - seqno;
            if offset >= 64 {
                return false; // too old
            }
            let mask = 1u64 << offset;
            (self.window & mask) == 0 // false if duplicate
        }
    }

    /// Update the window state to record that `seqno` has been received.
    /// Must only be called after the record has been authenticated (decrypted successfully).
    pub fn update(&mut self, seqno: u64) {
        if seqno > self.max_seq {
            let delta = seqno - self.max_seq;
            if delta > 63 {
                // Jump exceeds window size: clear entirely, only newest is seen
                self.window = 1;
            } else {
                self.window <<= delta;
                self.window |= 1; // mark newest as seen
            }
            self.max_seq = seqno;
        } else {
            let offset = self.max_seq - seqno;
            if offset < 64 {
                self.window |= 1u64 << offset;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: check and update in one step (simulates authenticated record).
    fn check_and_update(w: &mut ReplayWindow, seqno: u64) -> bool {
        if w.check(seqno) {
            w.update(seqno);
            true
        } else {
            false
        }
    }

    #[test]
    fn accepts_fresh_and_rejects_duplicate() {
        let mut w = ReplayWindow::new();
        assert!(check_and_update(&mut w, 1));
        assert!(!check_and_update(&mut w, 1)); // duplicate
        assert!(check_and_update(&mut w, 2)); // next fresh
    }

    #[test]
    fn accepts_out_of_order_within_window() {
        let mut w = ReplayWindow::new();
        assert!(check_and_update(&mut w, 10)); // establish max=10
        assert!(check_and_update(&mut w, 8)); // unseen within 64
        assert!(!check_and_update(&mut w, 8)); // duplicate now
        assert!(check_and_update(&mut w, 9)); // unseen within 64
    }

    #[test]
    fn rejects_too_old() {
        let mut w = ReplayWindow::new();
        assert!(check_and_update(&mut w, 100));
        // offset = 64 -> too old
        assert!(!check_and_update(&mut w, 36));
        // offset = 63 -> allowed once
        assert!(check_and_update(&mut w, 37));
    }

    #[test]
    fn handles_large_jump_and_window_shift() {
        let mut w = ReplayWindow::new();
        assert!(check_and_update(&mut w, 1));
        // Large forward jump clears the window entirely
        assert!(check_and_update(&mut w, 80));
        // Within window of new max and unseen
        assert!(check_and_update(&mut w, 79));
        // Too old relative to new max
        assert!(!check_and_update(&mut w, 15));
    }

    #[test]
    fn large_jump_does_not_leave_stale_bits() {
        let mut w = ReplayWindow::new();
        assert!(check_and_update(&mut w, 0));
        // Jump of 200 exceeds window size (64). The window must be fully
        // cleared so no stale bits from seq 0 remain.
        assert!(check_and_update(&mut w, 200));
        // seq 137 is within the window (offset = 200 - 137 = 63) and was
        // never seen, so it must be accepted.
        assert!(check_and_update(&mut w, 137));
    }

    #[test]
    fn check_does_not_modify_window() {
        let mut w = ReplayWindow::new();
        w.update(10);
        // check alone should not change state
        assert!(w.check(11));
        assert!(w.check(11)); // still acceptable because update was never called
        w.update(11);
        assert!(!w.check(11)); // now it's a duplicate
    }

    #[test]
    fn failed_auth_does_not_advance_window() {
        let mut w = ReplayWindow::new();
        w.update(5);
        // Simulate receiving seq 200 that passes check but fails authentication
        assert!(w.check(200));
        // Do NOT call update (authentication failed)
        // Legitimate packet at seq 6 should still be accepted
        assert!(w.check(6));
        w.update(6);
    }
}
