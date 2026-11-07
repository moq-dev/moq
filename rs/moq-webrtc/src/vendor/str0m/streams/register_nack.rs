use std::ops::Range;
use std::time::{Duration, Instant};

use crate::rtp_::{Nack, NackEntry, ReportList, SeqNo};

/// Number of out of order packets we keep track of for reports
const MAX_MISORDER: u64 = 100;

/// The max number of NACKs we perform for a single packet
const MAX_NACKS: u8 = 5;

pub(super) const DEFAULT_RTT: Duration = Duration::from_millis(100);
const NACK_RETRY_MARGIN: Duration = Duration::from_millis(5);

/// Circular buffer size
const BUFFER_SIZE: u64 = MAX_MISORDER + 1;

#[derive(Debug)]
pub struct NackRegister {
    /// Status of packets indexed by wrapping SeqNo.
    packets: Vec<PacketStatus>,

    /// Range of seq numbers considered NACK reporting.
    active: Option<Range<SeqNo>>,
}

#[derive(Debug, Default, Clone, Copy)]
struct PacketStatus {
    received: bool,
    nack_count: u8,
    last_nack_at: Option<Instant>,
}

impl PacketStatus {
    fn needs_nack(&self) -> bool {
        !self.received && self.nack_count < MAX_NACKS
    }

    fn is_nack_due(&self, now: Instant, retry_interval: Duration) -> bool {
        self.needs_nack()
            && self
                .last_nack_at
                .is_none_or(|last| now.saturating_duration_since(last) >= retry_interval)
    }

    fn mark_nacked(&mut self, now: Instant) {
        self.nack_count += 1;
        self.last_nack_at = Some(now);
    }

    fn mark_received(&mut self) -> bool {
        let new = !self.received;
        self.received = true;
        self.nack_count = 0;
        self.last_nack_at = None;
        new
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

struct NackIterator<'a> {
    reg: &'a mut NackRegister,
    next: u64,
    end: u64,
    now: Instant,
    retry_interval: Duration,
}

impl<'a> Iterator for NackIterator<'a> {
    type Item = NackEntry;

    fn next(&mut self) -> Option<Self::Item> {
        self.next = (self.next..=self.end).find(|s| {
            self.reg
                .packet((*s).into())
                .is_nack_due(self.now, self.retry_interval)
        })?;

        let mut entry = NackEntry {
            pid: self.next as u16,
            blp: 0,
        };

        self.reg.packet_mut(self.next.into()).mark_nacked(self.now);
        self.next += 1;

        for (i, s) in (self.next..self.end).take(16).enumerate() {
            let packet = self.reg.packet_mut(s.into());
            if packet.is_nack_due(self.now, self.retry_interval) {
                packet.mark_nacked(self.now);
                entry.blp |= 1 << i
            }
            self.next += 1;
        }

        Some(entry)
    }
}

impl NackRegister {
    /// Creates a new register.
    ///
    /// The max_seq_no is to provide a starting point for ROC calculations.
    pub fn new(max_seq_no: Option<SeqNo>) -> Self {
        let mut n = NackRegister {
            packets: vec![PacketStatus::default(); BUFFER_SIZE as usize],
            active: None,
        };

        if let Some(seq) = max_seq_no {
            n.init_with_seq(seq);
        }

        n
    }

    pub fn accepts(&self, seq: SeqNo) -> bool {
        let Some(active) = self.active.clone() else {
            // if we don't have initialized, we do want the first packet.
            return true;
        };

        // behind the window
        if seq < active.start {
            return false;
        }

        !self.packet(seq).received || seq > active.end
    }

    pub fn update(&mut self, seq: SeqNo) -> bool {
        let Some(active) = self.active.clone() else {
            // automatically pick up the first seq number
            self.init_with_seq(seq);
            return true;
        };

        if seq < active.start {
            // skip old seq numbers, report as not new
            return false;
        }

        let new = !self.packet_mut(seq).received || seq > active.end;

        let end = active.end.max(seq);

        let start: SeqNo = {
            let min = end.saturating_sub(MAX_MISORDER);
            let mut start = (*active.start).max(min);
            while start < *end {
                if !self.packet_mut(start.into()).received && start != *seq {
                    break;
                }
                start += 1;
            }
            start.into()
        };

        // reset packets that are rolling our of the nack window
        for (i, s) in (*active.start..*start).enumerate() {
            let p = self.packet_mut(s.into());
            if !p.received && s != *seq {
                trace!("Seq no {} missing after {} attempts", s, p.nack_count);
            }
            self.packet_mut(s.into()).reset();

            if i > self.packets.len() {
                // we have reset all entries already
                break;
            }
        }

        if (start..=end).contains(&seq) {
            self.packet_mut(seq).mark_received();
        }

        self.active = Some(start..end);

        new
    }

    fn init_with_seq(&mut self, seq: SeqNo) {
        self.active = Some(seq..seq);
        self.packet_mut(seq).mark_received();
    }

    pub fn max_seq(&self) -> Option<SeqNo> {
        self.active.as_ref().map(|a| a.end)
    }

    /// Whether a missing packet still has NACK attempts remaining.
    ///
    /// Includes retries that are waiting for their RTT deadline.
    pub fn has_pending_nacks(&self) -> bool {
        let Some(active) = &self.active else {
            return false;
        };

        (*active.start..=*active.end).any(|seq| self.packet(seq.into()).needs_nack())
    }

    /// Create NACK reports for packets whose requests are due.
    ///
    /// First requests are immediately eligible. Retries wait one receive-stream RTT plus
    /// 5 ms, using 100 ms until RTT is known. Only included packets consume an attempt.
    pub fn nack_reports(
        &mut self,
        now: Instant,
        rtt: Option<Duration>,
    ) -> Option<impl Iterator<Item = Nack>> {
        let retry_interval = rtt.unwrap_or(DEFAULT_RTT).saturating_add(NACK_RETRY_MARGIN);
        let Range { start, end } = self.active.clone()?;
        let start =
            (*start..=*end).find(|s| self.packet((*s).into()).is_nack_due(now, retry_interval))?;

        Some(
            ReportList::lists_from_iter(NackIterator {
                reg: self,
                next: start,
                end: *end,
                now,
                retry_interval,
            })
            .into_iter()
            .map(|reports| {
                Nack {
                    sender_ssrc: 0.into(),
                    ssrc: 0.into(), // changed when sending
                    reports,
                }
            }),
        )
    }

    /// Deadline for abandoning missing packets in a candidate's preceding sequence range.
    /// None means a relevant packet still has NACK attempts remaining.
    pub fn recovery_deadline(
        &self,
        range: Range<SeqNo>,
        candidate_received: Instant,
        rtt: Option<Duration>,
    ) -> Option<Instant> {
        let grace = rtt.unwrap_or(DEFAULT_RTT).saturating_mul(3);
        let fallback = candidate_received.checked_add(grace);
        let Some(active) = &self.active else {
            return fallback;
        };
        let mut deadline = None;
        for seq in *range.start.max(active.start)..*range.end.min(active.end) {
            let packet = self.packet(seq.into());
            if packet.received {
                continue;
            }
            if packet.needs_nack() {
                return None;
            }
            let at = packet.last_nack_at?.checked_add(grace)?;
            deadline = Some(deadline.map_or(at, |previous: Instant| previous.max(at)));
        }
        // Gaps outside the bounded NACK window and missing tail markers have no final
        // request to time from. Give them a candidate-relative grace period instead.
        deadline.or(fallback)
    }

    fn as_index(&self, seq: SeqNo) -> usize {
        (*seq % self.packets.len() as u64) as usize
    }

    fn packet(&self, seq: SeqNo) -> &PacketStatus {
        let index = self.as_index(seq);
        &self.packets[index]
    }

    fn packet_mut(&mut self, seq: SeqNo) -> &mut PacketStatus {
        let index = self.as_index(seq);
        &mut self.packets[index]
    }
}

#[cfg(test)]
mod test {
    use std::ops::Range;
    use std::time::{Duration, Instant};

    use crate::rtp_::NackEntry;
    use crate::streams::register_nack::MAX_MISORDER;

    use super::NackRegister;

    #[test]
    fn recovery_waits_for_final_nack_and_three_rtts() {
        for rtt in [
            None,
            Some(Duration::from_millis(20)),
            Some(Duration::from_millis(400)),
        ] {
            let base = Instant::now();
            let measured = rtt.unwrap_or(Duration::from_millis(100));
            let mut reg = NackRegister::new(None);
            reg.update(10.into());
            reg.update(12.into());
            let mut now = base;
            for attempt in 0..5 {
                assert_eq!(reg.recovery_deadline(10.into()..12.into(), base, rtt), None);
                assert_eq!(nack_entries(&mut reg, now, rtt).len(), 1);
                if attempt < 4 {
                    now += measured + Duration::from_millis(5);
                }
            }
            let deadline = Some(now + measured * 3);
            // Include a received packet before the active window; it must not add a fallback wait.
            assert_eq!(
                reg.recovery_deadline(10.into()..12.into(), base, rtt),
                deadline
            );
            reg.update(14.into());
            assert_eq!(
                reg.recovery_deadline(10.into()..12.into(), base, rtt),
                deadline
            );
            assert_eq!(reg.recovery_deadline(10.into()..14.into(), base, rtt), None);
            reg.update(11.into());
            assert_eq!(
                reg.recovery_deadline(10.into()..12.into(), base, rtt),
                Some(base + measured * 3)
            );
        }
    }

    #[test]
    fn pending_nacks_follow_repair_and_retry_exhaustion() {
        let mut reg = NackRegister::new(None);
        assert!(!reg.has_pending_nacks());

        reg.update(10.into());
        assert!(!reg.has_pending_nacks());

        reg.update(12.into());
        assert!(reg.has_pending_nacks());
        reg.update(11.into());
        assert!(!reg.has_pending_nacks());

        reg.update(14.into());
        let mut now = Instant::now();
        for _ in 0..super::MAX_NACKS {
            assert!(reg.has_pending_nacks());
            let reports: Vec<_> = reg
                .nack_reports(now, None)
                .expect("pending retry")
                .collect();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].reports[0].pid, 13);
            now += Duration::from_millis(105);
        }
        assert!(!reg.has_pending_nacks());
        assert!(reg.nack_reports(now, None).is_none());
    }

    fn nack_entries(reg: &mut NackRegister, now: Instant, rtt: Option<Duration>) -> Vec<NackEntry> {
        reg.nack_reports(now, rtt)
            .into_iter()
            .flatten()
            .flat_map(|nack| nack.reports)
            .collect()
    }

    #[test]
    fn nack_retry_deadlines_and_limit() {
        for (rtt, interval_ms) in [
            (None, 105),
            (Some(Duration::ZERO), 5),
            (Some(Duration::from_millis(20)), 25),
            (Some(Duration::from_millis(250)), 255),
        ] {
            let mut reg = NackRegister::new(None);
            reg.update(10.into());
            reg.update(13.into());
            let now = Instant::now();
            let interval = Duration::from_millis(interval_ms);
            let expected = [NackEntry { pid: 11, blp: 1 }];

            for attempt in 0..super::MAX_NACKS {
                let due = now + interval * u32::from(attempt);
                if attempt > 0 {
                    assert!(nack_entries(&mut reg, due - Duration::from_nanos(1), rtt).is_empty());
                }
                assert!(reg.has_pending_nacks());
                assert_eq!(nack_entries(&mut reg, due, rtt), expected);
                assert!(nack_entries(&mut reg, due, rtt).is_empty());
                for seq in [11, 12] {
                    let packet = reg.packet(seq.into());
                    assert_eq!(packet.nack_count, attempt + 1);
                    assert_eq!(packet.last_nack_at, Some(due));
                }
            }

            assert!(!reg.has_pending_nacks());
            assert!(nack_entries(&mut reg, now + interval * 10, rtt).is_empty());
        }
    }

    #[test]
    fn nack_new_losses_and_retries_have_independent_deadlines() {
        let mut reg = NackRegister::new(None);
        let now = Instant::now();
        reg.update(10.into());
        reg.update(12.into());
        assert_eq!(
            nack_entries(&mut reg, now, None),
            [NackEntry { pid: 11, blp: 0 }]
        );

        reg.update(14.into());
        assert_eq!(
            nack_entries(&mut reg, now + Duration::from_millis(30), None),
            [NackEntry { pid: 13, blp: 0 }]
        );
        assert_eq!(reg.packet(11.into()).last_nack_at, Some(now));

        reg.update(16.into());
        // 11 is due again and 15 is new, but 13 must not be included in the bitmask yet.
        assert_eq!(
            nack_entries(&mut reg, now + Duration::from_millis(105), None),
            [NackEntry {
                pid: 11,
                blp: 0b1000
            }]
        );
        assert_eq!(reg.packet(13.into()).nack_count, 1);
        assert_eq!(
            reg.packet(13.into()).last_nack_at,
            Some(now + Duration::from_millis(30))
        );
        assert_eq!(
            nack_entries(&mut reg, now + Duration::from_millis(135), None),
            [NackEntry { pid: 13, blp: 0 }]
        );
        assert_eq!(
            nack_entries(&mut reg, now + Duration::from_millis(210), None),
            [NackEntry {
                pid: 11,
                blp: 0b1000
            }]
        );
        assert_eq!(reg.packet(11.into()).nack_count, 3);
        assert_eq!(reg.packet(15.into()).nack_count, 2);
    }

    #[test]
    fn nack_retry_uses_updated_rtt() {
        let mut reg = NackRegister::new(None);
        reg.update(10.into());
        reg.update(12.into());
        let now = Instant::now();
        assert_eq!(nack_entries(&mut reg, now, None).len(), 1);
        assert_eq!(
            nack_entries(
                &mut reg,
                now + Duration::from_millis(50),
                Some(Duration::from_millis(20))
            )
            .len(),
            1
        );
        let rtt = Some(Duration::from_millis(200));
        assert!(nack_entries(&mut reg, now + Duration::from_millis(254), rtt).is_empty());
        assert_eq!(
            nack_entries(&mut reg, now + Duration::from_millis(255), rtt).len(),
            1
        );
    }

    #[test]
    fn nack_repair_clears_only_recovered_packet_state() {
        let mut reg = NackRegister::new(None);
        reg.update(10.into());
        reg.update(14.into());
        let now = Instant::now();
        assert_eq!(
            nack_entries(&mut reg, now, None),
            [NackEntry { pid: 11, blp: 3 }]
        );
        reg.update(12.into());
        let packet = reg.packet(12.into());
        assert!(packet.received);
        assert_eq!(packet.nack_count, 0);
        assert_eq!(packet.last_nack_at, None);
        assert_eq!(
            nack_entries(&mut reg, now + Duration::from_millis(105), None),
            [NackEntry { pid: 11, blp: 0b10 }]
        );
    }

    #[test]
    fn nack_window_reuse_clears_retry_state() {
        let mut reg = NackRegister::new(None);
        reg.update(10.into());
        reg.update(12.into());
        let now = Instant::now();
        assert_eq!(nack_entries(&mut reg, now, None).len(), 1);
        for seq in 13..112 {
            reg.update(seq.into());
        }
        reg.update(113.into());
        assert_eq!(reg.as_index(11.into()), reg.as_index(112.into()));
        assert_eq!(reg.packet(112.into()).nack_count, 0);
        assert_eq!(reg.packet(112.into()).last_nack_at, None);
        assert_eq!(
            nack_entries(&mut reg, now, None),
            [NackEntry { pid: 112, blp: 0 }]
        );
        assert_not_dirty(&reg);
    }

    #[test]
    fn nack_retry_timing_across_sequence_rollover() {
        let mut reg = NackRegister::new(None);
        reg.update(65_534.into());
        reg.update(65_538.into());
        let now = Instant::now();
        assert_eq!(
            nack_entries(&mut reg, now, None),
            [NackEntry {
                pid: 65_535,
                blp: 3
            }]
        );
        reg.update(65_536.into());
        assert!(nack_entries(&mut reg, now + Duration::from_millis(104), None).is_empty());
        assert_eq!(
            nack_entries(&mut reg, now + Duration::from_millis(105), None),
            [NackEntry {
                pid: 65_535,
                blp: 0b10
            }]
        );
    }

    fn assert_update(
        reg: &mut NackRegister,
        seq: u64,
        expect_new: bool,
        expect_received: bool,
        expect_active: Range<u64>,
    ) {
        assert_eq!(
            reg.update(seq.into()),
            expect_new,
            "seq {} was expected to{} be new",
            seq,
            if expect_new { "" } else { " NOT" }
        );
        let active = reg.active.clone().expect("nack range");
        assert_eq!(
            reg.packet_mut(seq.into()).received,
            expect_received,
            "seq {} expected to{} be received in {:?}",
            seq,
            if expect_received { "" } else { " NOT" },
            active
        );
        assert_eq!(active, expect_active.start.into()..expect_active.end.into());
        assert_not_dirty(reg);
    }

    fn assert_not_dirty(reg: &NackRegister) {
        // we should leave no dirty state outside of the nack window
        let active = reg.active.clone().expect("nack range");
        let active = (*active.start..=*active.end)
            .map(|seq| reg.as_index(seq.into()))
            .collect::<Vec<_>>();

        for i in 0..reg.packets.len() {
            if active.contains(&i) {
                continue;
            }
            assert!(
                !reg.packets[i].received
                    && reg.packets[i].nack_count == 0
                    && reg.packets[i].last_nack_at.is_none(),
                "dirty state at index {} outside of nack window {:?}",
                i,
                active,
            );
        }
    }

    #[test]
    fn active_window_sliding() {
        let mut reg = NackRegister::new(None);

        assert!(reg.accepts(10.into()));
        assert_update(&mut reg, 10, true, true, 10..10);

        // packet before window start is ignored
        assert!(!reg.accepts(9.into()));
        assert_update(&mut reg, 9, false, false, 10..10);

        // duped packet
        assert!(!reg.accepts(10.into()));
        assert_update(&mut reg, 10, false, true, 10..10);

        // future packets accepted, window not sliding
        let next = 10 + MAX_MISORDER;
        assert!(reg.accepts(next.into()));
        assert_update(&mut reg, next, true, true, 11..next);
        let next = 11 + MAX_MISORDER;
        assert!(reg.accepts(next.into()));
        assert_update(&mut reg, next, true, true, 11..next);

        // future packet accepted, sliding window
        let next = 12 + MAX_MISORDER;
        assert!(reg.accepts(next.into()));
        assert_update(&mut reg, next, true, true, 12..next);

        // older packet received within window
        let next = 13;
        assert!(reg.accepts(next.into()));
        assert_update(&mut reg, next, true, true, 12..(12 + MAX_MISORDER));

        // do not want the same packet again
        assert!(!reg.accepts(next.into()));

        // future packet accepted, sliding window start skips over received
        let next = 13 + MAX_MISORDER;
        assert!(reg.accepts(next.into()));
        assert_update(&mut reg, next, true, true, 14..next);

        // do not want the same packet again
        assert!(!reg.accepts(next.into()));

        // older packet accepted, window star moves ahead
        let next = 14;
        assert!(reg.accepts(next.into()));
        assert_update(&mut reg, next, true, false, 15..(13 + MAX_MISORDER));
    }

    #[test]
    fn nack_report_none() {
        let mut reg = NackRegister::new(None);
        assert!(reg.nack_reports(Instant::now(), None).is_none());

        reg.update(110.into());
        assert!(reg.nack_reports(Instant::now(), None).is_none());

        reg.update(111.into());
        assert!(reg.nack_reports(Instant::now(), None).is_none());
    }

    #[test]
    fn nack_test_huge_seq_gap_no_hang() {
        let mut reg = NackRegister::new(None);

        reg.update(0.into());
        reg.update(18446744073709551515.into());
    }

    #[test]
    fn nack_report_one() {
        let mut reg = NackRegister::new(None);
        assert!(reg.nack_reports(Instant::now(), None).is_none());

        reg.update(110.into());
        assert!(reg.nack_reports(Instant::now(), None).is_none());

        reg.update(112.into());
        let report = reg
            .nack_reports(Instant::now(), None)
            .map(Vec::from_iter)
            .expect("some report");
        assert!(report.len() == 1);
        assert_eq!(report[0].reports.len(), 1);
        assert_eq!(report[0].reports[0].pid, 111);
        assert_eq!(report[0].reports[0].blp, 0);
    }

    #[test]
    fn nack_report_two() {
        let mut reg = NackRegister::new(None);
        assert!(reg.nack_reports(Instant::now(), None).is_none());

        reg.update(110.into());
        assert!(reg.nack_reports(Instant::now(), None).is_none());

        reg.update(113.into());
        let report = reg
            .nack_reports(Instant::now(), None)
            .map(Vec::from_iter)
            .expect("some report");
        assert!(report.len() == 1);
        assert_eq!(report[0].reports.len(), 1);
        assert_eq!(report[0].reports[0].pid, 111);
        assert_eq!(report[0].reports[0].blp, 0b1);
    }

    #[test]
    fn nack_report_with_hole() {
        let mut reg = NackRegister::new(None);

        for i in &[100, 101, 103, 105, 106, 107, 108, 109, 110] {
            reg.update((*i).into());
        }

        let report = reg
            .nack_reports(Instant::now(), None)
            .map(Vec::from_iter)
            .expect("some report");
        assert!(report.len() == 1);
        assert_eq!(report[0].reports.len(), 1);
        assert_eq!(report[0].reports[0].pid, 102);
        assert_eq!(report[0].reports[0].blp, 0b10);
    }

    #[test]
    fn nack_report_stop_at_17() {
        let mut reg = NackRegister::new(None);

        let seq = &[
            100, 101, 103, 104, 105, 106, 107, 108, 109, 110, //
            111, 112, 113, 114, 115, 116, 117, 118, 119, 120, //
            121, 122, 123, 125,
        ];

        for i in seq {
            reg.update((*i).into());
        }

        let report = reg
            .nack_reports(Instant::now(), None)
            .map(Vec::from_iter)
            .expect("some report");
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].reports.len(), 2);
        assert_eq!(report[0].reports[0].pid, 102);
        assert_eq!(report[0].reports[0].blp, 0);
    }

    #[test]
    fn nack_report_hole_at_17() {
        let mut reg = NackRegister::new(None);

        let seq = &[
            100, 101, 103, 104, 105, 106, 107, 108, 109, 110, //
            111, 112, 113, 114, 115, 116, 117, 119, 120, 121, //
            122, 123, 124, 125, 126, 127, 128, 129,
        ];

        for i in seq {
            reg.update((*i).into());
        }

        let report = reg
            .nack_reports(Instant::now(), None)
            .map(Vec::from_iter)
            .expect("some report");
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].reports.len(), 1);
        assert_eq!(report[0].reports[0].pid, 102);
        assert_eq!(report[0].reports[0].blp, 0b1000_0000_0000_0000);
    }

    #[test]
    fn nack_report_no_stop_all_there() {
        let mut reg = NackRegister::new(None);

        let seq = &[
            100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, //
            111, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121, //
            122, 123, 124, 125, 126, 127, 128, 129,
        ];

        for i in seq {
            reg.update((*i).into());
        }

        assert!(reg.nack_reports(Instant::now(), None).is_none());
    }

    #[test]
    fn nack_report_rtx() {
        let mut reg = NackRegister::new(None);
        for i in &[
            100, 101, 102, 103, 104, 105, //
        ] {
            reg.update((*i).into());
        }
        assert!(reg.nack_reports(Instant::now(), None).is_none());
        let active = reg.active.clone().expect("nack range");
        assert_eq!(*active.start, 105);

        for i in &[
            106, 108, 109, 110, 111, 112, 113, 114, 115, //
        ] {
            reg.update((*i).into());
        }
        assert!(reg.nack_reports(Instant::now(), None).is_some());
        let active = reg.active.clone().expect("nack range");
        assert_eq!(*active.start, 107);

        reg.update(107.into()); // Got 107 via RTX

        let nacks = reg.nack_reports(Instant::now(), None).map(Vec::from_iter);
        assert!(
            nacks.is_none(),
            "Expected no NACKs to be generated after repairing the stream, got {nacks:?}"
        );
        let active = reg.active.clone().expect("nack range");
        assert_eq!(*active.start, 115);
    }

    #[test]
    fn nack_report_rollover_rtx() {
        // This test is checking that after rollover nacks are not skipped because of
        // packet position that would remain marked as received from before the rollover
        let mut reg = NackRegister::new(None);
        for i in &[
            100, 101, 102, 103, 104, 105, 106, 108, 109, 110, 111, 112, 113, 114, 115,
        ] {
            reg.update((*i).into());
        }

        reg.update(107.into()); // Got 107 via RTX
        let active = reg.active.clone().expect("nack range");
        assert_eq!(*active.start, 115);

        for i in 116..3106 {
            reg.update(i.into());
        }
        let active = reg.active.clone().expect("nack range");
        assert_eq!(*active.start, 3105);

        for i in &[3106, 3108, 3109, 3110, 3111, 3112, 3113, 3114, 3115] {
            reg.update((*i).into()); // Missing at postion 107 again
        }

        let active = reg.active.clone().expect("nack range");
        assert_eq!(*active.start, 3107);
    }

    #[test]
    fn nack_report_rollover_rtx_with_seq_jump() {
        let mut reg = NackRegister::new(None);

        // 2999 is missing
        for i in 0..2999 {
            reg.update(i.into());
        }

        // 3002 is missing
        reg.update(3003.into());
        reg.update(3004.into());
        reg.update(3000.into());
        reg.update(3001.into());

        let reports = reg
            .nack_reports(Instant::now(), None)
            .map(Vec::from_iter)
            .expect("some report");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].reports[0].pid, 2999);
        assert_eq!(reports[0].reports[0].blp, 4);
    }

    #[test]
    fn out_of_order_and_rollover() {
        let mut reg = NackRegister::new(None);

        reg.update(2998.into());
        reg.update(2999.into());

        // receive older packet
        reg.update(2995.into());

        // wrap
        for i in 3000..5995 {
            reg.update(i.into());
        }

        // 5995 is missing

        reg.update(5996.into());
        reg.update(5997.into());

        let reports = reg
            .nack_reports(Instant::now(), None)
            .map(Vec::from_iter)
            .expect("some report");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].reports[0].pid, 5995);
    }

    #[test]
    fn nack_check_on_seq_rollover() {
        let range = 65530..65541;
        let missing = [65535_u64, 65536_u64, 65537_u64];
        let expected = [65535_u16, 0_u16, 1_u16];

        for (missing, expected) in missing.iter().zip(expected.iter()) {
            let mut seqs: Vec<_> = range.clone().collect();
            let mut reg = NackRegister::new(None);

            seqs.retain(|x| *x != *missing);
            for i in seqs.as_slice() {
                reg.update((*i).into());
            }

            let reports = reg
                .nack_reports(Instant::now(), None)
                .map(Vec::from_iter)
                .expect("some report");
            let pid = reports[0].reports[0].pid;
            assert_eq!(pid, *expected);
        }
    }

    #[test]
    fn nack_check_forward_at_boundary() {
        let mut reg = NackRegister::new(None);
        for i in 2996..=3003 {
            reg.update(i.into());
        }

        assert!(reg.nack_reports(Instant::now(), None).is_none());
        let active = reg.active.clone().expect("nack range");
        assert_eq!(*active.start, 3003);

        for i in 3004..=3008 {
            reg.update(i.into());
        }

        let report = reg.nack_reports(Instant::now(), None).map(Vec::from_iter);
        assert!(report.is_none(), "Expected empty NACKs got {:?}", report);
        let active = reg.active.clone().expect("nack range");
        assert_eq!(*active.start, 3008);
    }

    #[test]
    fn nack_check_forward_at_u16_boundary() {
        let mut reg = NackRegister::new(None);
        for i in 65500..=65534 {
            reg.update(i.into());
        }
        assert!(reg.nack_reports(Instant::now(), None).is_none());
        let active = reg.active.clone().expect("nack range");
        assert_eq!(*active.start, 65534);

        for i in 65536..=65566 {
            reg.update(i.into());
        }

        assert!(reg.nack_reports(Instant::now(), None).is_some());
        let active = reg.active.clone().expect("nack range");
        assert_eq!(*active.start, 65535);

        for i in 65567..=65666 {
            reg.update(i.into());
        }

        reg.update(65535.into());

        assert!(reg.nack_reports(Instant::now(), None).is_none());
        let active = reg.active.clone().expect("nack range");
        assert_eq!(*active.start, 65666);
    }

    #[test]
    fn nack_reports_on_boundaries() {
        let mut reg = NackRegister::new(None);

        for i in 0..=20 {
            // gap must be at least 17 packets to separate the reports.
            if i == 2 || i == 19 {
                continue;
            }
            reg.update(i.into());
        }

        let reports: Vec<_> = reg
            .nack_reports(Instant::now(), None)
            .expect("should generate reports")
            .flat_map(|nack| nack.reports)
            .collect();

        assert_eq!(reports.len(), 2, "Should have found two NACK entries");
        assert_eq!(reports[0].pid, 2, "First missing packet should be 2");
        assert_eq!(
            reports[0].blp, 0,
            "No missing packets in blp bits following 2"
        );
        assert_eq!(reports[1].pid, 19, "Second missing packet should be 19");
        assert_eq!(
            reports[1].blp, 0,
            "No missing packets in blp bits following 19"
        );
    }
}
