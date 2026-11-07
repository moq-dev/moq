use core::time::Duration;
use std::time::Instant;

use crate::config::{RTO_INITIAL, RTO_MAX, RTO_MIN};

pub(crate) const ACK_INTERVAL: u64 = 200;
const TIMER_COUNT: usize = 6;

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub(crate) enum Timer {
    T1Init = 0,
    T1Cookie = 1,
    T2Shutdown = 2,
    T3RTX = 3,
    Reconfig = 4,
    Ack = 5,
}

impl Timer {
    pub(crate) const VALUES: [Self; TIMER_COUNT] = [
        Timer::T1Init,
        Timer::T1Cookie,
        Timer::T2Shutdown,
        Timer::T3RTX,
        Timer::Reconfig,
        Timer::Ack,
    ];
}

/// A table of data associated with each distinct kind of `Timer`
#[derive(Debug, Copy, Clone)]
pub(crate) struct TimerTable {
    data: [Option<Instant>; TIMER_COUNT],
    retrans: [usize; TIMER_COUNT],
    /// Maximum retransmissions for each timer. `None` means unlimited.
    max_retrans: [Option<usize>; TIMER_COUNT],
    /// Timers whose next expiry must not increment the error counter
    no_error_count: [bool; TIMER_COUNT],
    /// Maximum RTO value for exponential backoff.
    rto_max: u64,
}

impl Default for TimerTable {
    fn default() -> Self {
        TimerTable {
            data: [None; TIMER_COUNT],
            retrans: [0; TIMER_COUNT],
            max_retrans: [None; TIMER_COUNT],
            no_error_count: [false; TIMER_COUNT],
            rto_max: 60000, // Default RTO_MAX
        }
    }
}

impl TimerTable {
    pub fn new(
        max_init_retransmits: Option<usize>,
        max_data_retransmits: Option<usize>,
        rto_max: u64,
    ) -> Self {
        TimerTable {
            max_retrans: [
                max_init_retransmits, //T1Init
                max_init_retransmits, //T1Cookie
                None,                 //T2Shutdown (unlimited)
                max_data_retransmits, //T3RTX
                max_init_retransmits, //Reconfig
                None,                 //Ack (unlimited)
            ],
            rto_max,
            ..Default::default()
        }
    }

    pub fn set(&mut self, timer: Timer, time: Option<Instant>) {
        self.data[timer as usize] = time;
    }

    pub fn get(&self, timer: Timer) -> Option<Instant> {
        self.data[timer as usize]
    }

    pub fn next_timeout(&self) -> Option<Instant> {
        self.data.iter().filter_map(|&x| x).min()
    }

    pub fn start(&mut self, timer: Timer, now: Instant, interval: u64) {
        let interval = if timer == Timer::Ack {
            interval
        } else {
            calculate_next_timeout(interval, self.retrans[timer as usize], self.rto_max)
        };

        let time = now + Duration::from_millis(interval);
        self.data[timer as usize] = Some(time);
    }

    /// Restarts the timer if the current instant is none or elapsed.
    pub fn restart_if_stale(&mut self, timer: Timer, now: Instant, interval: u64) {
        if let Some(current) = self.data[timer as usize] {
            if current >= now {
                return;
            }
        }

        self.start(timer, now, interval);
    }

    pub fn stop(&mut self, timer: Timer) {
        self.data[timer as usize] = None;
        self.retrans[timer as usize] = 0;
        self.no_error_count[timer as usize] = false;
    }

    /// Clear peer error accounting without changing the recovery deadline.
    pub fn reset_retrans(&mut self, timer: Timer) {
        self.retrans[timer as usize] = 0;
    }

    /// Exempt the timer's next expiry from the `max_retrans` accounting.
    pub fn suppress_error_count(&mut self, timer: Timer) {
        self.no_error_count[timer as usize] = true;
    }

    pub fn is_expired(&mut self, timer: Timer, after: Instant) -> (bool, bool, usize) {
        let expired = self.data[timer as usize].is_some_and(|x| x <= after);
        let mut failure = false;
        if expired {
            if self.no_error_count[timer as usize] {
                self.no_error_count[timer as usize] = false;
            } else {
                self.retrans[timer as usize] += 1;
                if let Some(max) = self.max_retrans[timer as usize] {
                    if self.retrans[timer as usize] > max {
                        failure = true;
                    }
                }
                // If max_retrans is None, failure stays false (unlimited)
            }
        }

        (expired, failure, self.retrans[timer as usize])
    }
}

const RTO_ALPHA: u64 = 1;
const RTO_BETA: u64 = 2;
const RTO_BASE: u64 = 8;

/// rtoManager manages Rtx timeout values.
/// This is an implementation of RFC 4960 sec 6.3.1.
#[derive(Debug)]
pub(crate) struct RtoManager {
    pub(crate) srtt: u64,
    pub(crate) rttvar: f64,
    pub(crate) rto: u64,
    pub(crate) no_update: bool,
    pub(crate) rto_initial: u64,
    pub(crate) rto_min: u64,
    pub(crate) rto_max: u64,
}

impl Default for RtoManager {
    fn default() -> Self {
        RtoManager {
            srtt: 0,
            rttvar: 0.0,
            rto: RTO_INITIAL,
            no_update: false,
            rto_initial: RTO_INITIAL,
            rto_min: RTO_MIN,
            rto_max: RTO_MAX,
        }
    }
}

impl RtoManager {
    /// Creates a new RtoManager with configurable RTO values.
    pub(crate) fn new(rto_initial: u64, rto_min: u64, rto_max: u64) -> Self {
        RtoManager {
            srtt: 0,
            rttvar: 0.0,
            rto: rto_initial,
            no_update: false,
            rto_initial,
            rto_min,
            rto_max,
        }
    }

    /// set_new_rtt takes a newly measured RTT then adjust the RTO in msec.
    pub(crate) fn set_new_rtt(&mut self, rtt: u64) -> u64 {
        if self.no_update {
            return self.srtt;
        }

        if self.srtt == 0 {
            // First measurement
            self.srtt = rtt;
            self.rttvar = rtt as f64 / 2.0;
        } else {
            // Subsequent rtt measurement
            self.rttvar = ((RTO_BASE - RTO_BETA) as f64 * self.rttvar
                + RTO_BETA as f64 * (self.srtt as i64 - rtt as i64).abs() as f64)
                / RTO_BASE as f64;
            self.srtt = ((RTO_BASE - RTO_ALPHA) * self.srtt + RTO_ALPHA * rtt) / RTO_BASE;
        }

        self.rto = (self.srtt + (4.0 * self.rttvar) as u64).clamp(self.rto_min, self.rto_max);

        self.srtt
    }

    /// get_rto simply returns the current RTO in msec.
    pub(crate) fn get_rto(&self) -> u64 {
        self.rto
    }

    /// reset resets the RTO variables to the initial values.
    pub(crate) fn reset(&mut self) {
        if self.no_update {
            return;
        }

        self.srtt = 0;
        self.rttvar = 0.0;
        self.rto = self.rto_initial;
    }

    /// set RTO value for testing
    pub(crate) fn set_rto(&mut self, rto: u64, no_update: bool) {
        self.rto = rto;
        self.no_update = no_update;
    }
}

fn calculate_next_timeout(rto: u64, n_rtos: usize, rto_max: u64) -> u64 {
    // RFC 4096 sec 6.3.3.  Handle T3-rtx Expiration
    //   E2)  For the destination address for which the timer expires, set RTO
    //        <- RTO * 2 ("back off the timer").  The maximum value discussed
    //        in rule C7 above (RTO.max) may be used to provide an upper bound
    //        to this doubling operation.
    if n_rtos < 31 {
        core::cmp::min(rto << n_rtos, rto_max)
    } else {
        rto_max
    }
}
