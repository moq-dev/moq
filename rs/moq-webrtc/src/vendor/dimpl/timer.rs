use std::time::{Duration, Instant};

use crate::{Config, SeededRng, TimeoutError};

const JITTER_RANGE: f32 = 0.5;
const IDLE_INTERVAL: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

pub struct HandshakeTimers {
    handshake_timeout: Duration,
    handshake: Timeout,
    flight: Timeout,
    backoff: ExponentialBackoff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeout {
    Disabled,
    Unarmed,
    Armed(Instant),
}

pub struct ExponentialBackoff {
    start_rto: Duration,
    retries: usize,
    rto: Duration,
    jitter: f32,
    left: usize,
}

impl HandshakeTimers {
    pub fn new(config: &Config, rng: &mut SeededRng) -> Self {
        Self {
            handshake_timeout: config.handshake_timeout(),
            handshake: Timeout::Unarmed,
            flight: Timeout::Disabled,
            backoff: ExponentialBackoff::new(
                config.flight_start_rto(),
                config.flight_retries(),
                rng,
            ),
        }
    }

    pub fn start_handshake(&mut self, now: Instant) {
        if self.handshake == Timeout::Unarmed {
            self.handshake = Timeout::Armed(deadline(now, self.handshake_timeout));
        }
    }

    pub fn handshake_deadline(&self) -> Timeout {
        self.handshake
    }

    pub fn set_handshake_deadline(&mut self, timeout: Timeout) {
        self.handshake = timeout;
    }

    pub fn begin_flight(&mut self, rng: &mut SeededRng) {
        self.backoff.reset(rng);
        self.flight = Timeout::Unarmed;
    }

    pub fn flight_sent(&mut self, now: Instant) {
        if self.flight == Timeout::Unarmed {
            self.flight = Timeout::Armed(deadline(now, self.backoff.rto()));
        }
    }

    pub fn stop_flight(&mut self) {
        self.flight = Timeout::Disabled;
    }

    pub fn finish_handshake(&mut self) {
        self.handshake = Timeout::Disabled;
    }

    pub fn stop(&mut self) {
        self.finish_handshake();
        self.stop_flight();
    }

    pub fn rto(&self) -> Duration {
        self.backoff.rto()
    }

    /// Reserve a retry for a saved flight. Emission arms its next timeout.
    ///
    /// After the handshake finishes, resending the final flight in answer to
    /// peer retransmissions ignores the remaining budget (RFC 6347 §4.2.4).
    pub fn request_resend(&mut self, rng: &mut SeededRng) -> bool {
        let final_flight = self.handshake == Timeout::Disabled && self.flight == Timeout::Disabled;
        if self.flight == Timeout::Unarmed || !(final_flight || self.backoff.can_retry()) {
            return false;
        }
        self.backoff.attempt(rng);
        if matches!(self.flight, Timeout::Armed(_)) {
            self.flight = Timeout::Unarmed;
        }
        true
    }

    pub fn handle_timeout(
        &mut self,
        now: Instant,
        rng: &mut SeededRng,
    ) -> Result<bool, TimeoutError> {
        if let Timeout::Armed(timeout) = self.handshake
            && now >= timeout
        {
            return Err(TimeoutError::Connect);
        }
        if let Timeout::Armed(timeout) = self.flight
            && now >= timeout
        {
            if !self.request_resend(rng) {
                return Err(TimeoutError::Handshake);
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub fn poll_timeout(&self, now: Instant) -> Instant {
        match (self.handshake, self.flight) {
            (Timeout::Armed(connect), Timeout::Armed(flight)) => connect.min(flight),
            (Timeout::Armed(timeout), _) | (_, Timeout::Armed(timeout)) => timeout,
            _ => deadline(now, IDLE_INTERVAL),
        }
    }
}

impl ExponentialBackoff {
    pub fn new(start_rto: Duration, retries: usize, rng: &mut SeededRng) -> Self {
        Self {
            start_rto,
            retries,
            rto: start_rto,
            jitter: Self::jitter(rng),
            left: retries,
        }
    }

    pub fn reset(&mut self, rng: &mut SeededRng) {
        self.rto = self.start_rto;
        self.jitter = Self::jitter(rng);
        self.left = self.retries;
    }

    pub fn rto(&self) -> Duration {
        let jitter = self.rto.mul_f64(f64::from(self.jitter.abs()));
        if self.jitter < 0.0 {
            self.rto.saturating_sub(jitter)
        } else {
            self.rto.saturating_add(jitter)
        }
        .max(Duration::from_nanos(1))
    }

    fn jitter(rng: &mut SeededRng) -> f32 {
        rng.random::<f32>() * JITTER_RANGE - (JITTER_RANGE / 2.0)
    }

    pub fn attempt(&mut self, rng: &mut SeededRng) {
        let (n, overflow) = self.left.overflowing_sub(1);

        if overflow {
            return;
        }

        self.left = n;
        self.jitter = Self::jitter(rng);
        self.rto = self.rto.saturating_mul(2);
    }

    pub fn can_retry(&self) -> bool {
        self.left > 0
    }
}

/// Add a logical delay, reducing unrepresentable intervals until they fit.
pub fn deadline(now: Instant, mut delay: Duration) -> Instant {
    loop {
        if let Some(deadline) = now.checked_add(delay) {
            return deadline;
        }
        delay /= 2;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn timers(retries: usize) -> (HandshakeTimers, SeededRng) {
        let config = Config::builder()
            .handshake_timeout(Duration::from_secs(2))
            .flight_start_rto(Duration::from_millis(20))
            .flight_retries(retries)
            .build()
            .expect("valid timing config");
        let mut rng = SeededRng::new(Some(42));
        (HandshakeTimers::new(&config, &mut rng), rng)
    }

    #[test]
    fn idle_and_unsent_flights_do_not_run_clocks() {
        let (mut timers, mut rng) = timers(0);
        let now = Instant::now();
        assert!(timers.poll_timeout(now) > now);
        assert_eq!(timers.handle_timeout(now, &mut rng), Ok(false));
        timers.flight_sent(now);
        assert_eq!(timers.flight, Timeout::Disabled);
        timers.begin_flight(&mut rng);
        let late = now + Duration::from_secs(100);
        assert_eq!(timers.handle_timeout(late, &mut rng), Ok(false));
        assert!(timers.poll_timeout(late) > late);
        assert_eq!(timers.handshake_deadline(), Timeout::Unarmed);
        timers.start_handshake(late);
        assert_eq!(timers.poll_timeout(late), late + Duration::from_secs(2));
        timers.flight_sent(late);
        let retry_at = late + timers.rto();
        assert_eq!(timers.poll_timeout(late), retry_at);
        assert_eq!(timers.handle_timeout(late, &mut rng), Ok(false));
        assert_eq!(
            timers.handle_timeout(retry_at, &mut rng),
            Err(TimeoutError::Handshake)
        );
    }

    #[test]
    fn flights_fragments_and_handoffs_preserve_deadline() {
        let (mut timers, mut rng) = timers(1);
        let now = Instant::now();
        timers.start_handshake(now);
        let original = timers.handshake_deadline();
        timers.begin_flight(&mut rng);
        timers.flight_sent(now);
        let first_retry = timers.poll_timeout(now);
        timers.flight_sent(now + Duration::from_millis(1));
        assert_eq!(timers.poll_timeout(now), first_retry);
        assert_eq!(timers.handle_timeout(first_retry, &mut rng), Ok(true));
        timers.flight_sent(first_retry);
        let last_retry = timers.poll_timeout(first_retry);
        assert_eq!(
            timers.handle_timeout(last_retry, &mut rng),
            Err(TimeoutError::Handshake)
        );
        timers.begin_flight(&mut rng);
        assert_eq!(
            timers.poll_timeout(last_retry),
            now + Duration::from_secs(2)
        );
        timers.flight_sent(last_retry);
        assert!(timers.poll_timeout(last_retry) < now + Duration::from_secs(2));
        timers.start_handshake(last_retry);
        assert_eq!(timers.handshake_deadline(), original);
        timers.stop_flight();
        assert_eq!(
            timers.poll_timeout(last_retry),
            now + Duration::from_secs(2)
        );

        let config = Config::default();
        let mut inherited = HandshakeTimers::new(&config, &mut rng);
        inherited.set_handshake_deadline(original);
        inherited.start_handshake(last_retry);
        assert_eq!(
            inherited.poll_timeout(last_retry),
            now + Duration::from_secs(2)
        );
        assert_eq!(
            inherited.handle_timeout(now + Duration::from_secs(2), &mut rng),
            Err(TimeoutError::Connect)
        );
    }

    #[test]
    fn overall_deadline_wins_over_flight_deadline() {
        let (mut timers, mut rng) = timers(usize::MAX);
        let now = Instant::now();
        timers.start_handshake(now);
        timers.begin_flight(&mut rng);
        timers.backoff.rto = Duration::from_secs(10);
        timers.flight_sent(now);
        assert_eq!(timers.poll_timeout(now), now + Duration::from_secs(2));
        assert_eq!(
            timers.handle_timeout(now + Duration::from_secs(2), &mut rng),
            Err(TimeoutError::Connect)
        );
    }

    #[test]
    fn completion_disables_handshake_but_allows_key_update_flights() {
        let (mut timers, mut rng) = timers(1);
        let now = Instant::now();
        timers.start_handshake(now);
        timers.begin_flight(&mut rng);
        timers.flight_sent(now);
        timers.finish_handshake();
        assert_eq!(timers.poll_timeout(now), now + timers.rto());
        timers.stop();
        let later = now + Duration::from_secs(100);
        timers.start_handshake(later);
        timers.flight_sent(later);
        assert_eq!(timers.handshake_deadline(), Timeout::Disabled);
        assert!(timers.poll_timeout(later) > later);
        assert_eq!(timers.handle_timeout(later, &mut rng), Ok(false));
        timers.begin_flight(&mut rng);
        timers.flight_sent(later);
        assert_eq!(timers.poll_timeout(later), later + timers.rto());
        assert_eq!(
            timers.handle_timeout(later + timers.rto(), &mut rng),
            Ok(true)
        );
        timers.stop_flight();
        assert!(timers.poll_timeout(later) > later);
    }

    #[test]
    fn deadline_overflow_does_not_panic() {
        let now = Instant::now();
        assert_eq!(deadline(now, Duration::ZERO), now);
        assert_eq!(
            deadline(now, Duration::from_nanos(1)),
            now + Duration::from_nanos(1)
        );
        assert!(now.checked_add(Duration::MAX).is_none());
        assert!(deadline(now, Duration::MAX) > now);
    }

    #[test]
    fn duplicate_and_timed_resends_share_attempts() {
        let (mut timers, mut rng) = timers(2);
        let now = Instant::now();
        timers.start_handshake(now);
        timers.begin_flight(&mut rng);
        assert!(!timers.request_resend(&mut rng));
        assert_eq!(timers.backoff.left, 2);
        timers.flight_sent(now);
        let first_retry = timers.poll_timeout(now);
        assert_eq!(timers.handle_timeout(first_retry, &mut rng), Ok(true));
        assert_eq!(timers.backoff.left, 1);
        assert_eq!(timers.flight, Timeout::Unarmed);
        timers.flight_sent(first_retry);
        assert!(timers.request_resend(&mut rng));
        assert_eq!(timers.backoff.left, 0);
        assert!(!timers.request_resend(&mut rng));
        assert_eq!(
            timers.poll_timeout(first_retry),
            now + Duration::from_secs(2)
        );
        timers.flight_sent(first_retry);
        let exhausted = timers.poll_timeout(first_retry);
        assert_eq!(
            timers.handle_timeout(exhausted, &mut rng),
            Err(TimeoutError::Handshake)
        );
    }

    #[test]
    fn final_flight_resends_ignore_budget_without_restarting_timers() {
        let (mut timers, mut rng) = timers(0);
        let now = Instant::now();
        timers.start_handshake(now);
        timers.begin_flight(&mut rng);
        timers.flight_sent(now);
        timers.stop();
        for _ in 0..3 {
            assert!(timers.request_resend(&mut rng));
            timers.flight_sent(now);
        }
        assert_eq!(timers.handshake_deadline(), Timeout::Disabled);
        assert_eq!(timers.flight, Timeout::Disabled);
        assert_eq!(
            timers.handle_timeout(now + Duration::from_secs(100), &mut rng),
            Ok(false)
        );
    }

    #[test]
    fn stopped_flight_keeps_budget_until_handshake_finishes() {
        let (mut timers, mut rng) = timers(1);
        let now = Instant::now();
        timers.start_handshake(now);
        timers.begin_flight(&mut rng);
        timers.flight_sent(now);
        timers.stop_flight();
        assert!(timers.request_resend(&mut rng));
        assert!(!timers.request_resend(&mut rng));
        timers.finish_handshake();
        assert!(timers.request_resend(&mut rng));
    }

    #[test]
    fn proportional_jitter() {
        let mut rng = SeededRng::new(Some(42));
        for rto in [
            Duration::from_nanos(4),
            Duration::from_micros(100),
            Duration::from_millis(20),
            Duration::from_secs(1),
            Duration::from_secs(100),
        ] {
            let mut exp = ExponentialBackoff::new(rto, 1, &mut rng);
            exp.jitter = -0.25;
            assert_eq!(exp.rto(), rto - rto / 4);
            exp.jitter = 0.0;
            assert_eq!(exp.rto(), rto);
            exp.jitter = 0.25;
            assert_eq!(exp.rto(), rto + rto / 4);
        }
    }

    #[test]
    fn smallest_rto_stays_positive() {
        let mut rng = SeededRng::new(Some(42));
        let mut exp = ExponentialBackoff::new(Duration::from_nanos(1), 1, &mut rng);
        for jitter in [-0.25, 0.0, 0.25] {
            exp.jitter = jitter;
            assert_eq!(exp.rto(), Duration::from_nanos(1));
        }
    }

    #[test]
    fn overflow_saturates() {
        let mut rng = SeededRng::new(Some(42));
        let mut exp = ExponentialBackoff::new(Duration::MAX, usize::MAX, &mut rng);
        exp.jitter = 0.25;
        assert_eq!(exp.rto(), Duration::MAX);
        exp.attempt(&mut rng);
        assert_eq!(exp.rto, Duration::MAX);
        assert_eq!(exp.left, usize::MAX - 1);
        exp.jitter = -0.25;
        assert!(exp.rto() < Duration::MAX);
        assert!(exp.rto() > Duration::MAX / 2);
    }

    #[test]
    fn zero_retries_and_reset() {
        let mut rng = SeededRng::new(Some(42));
        let start = Duration::from_millis(20);
        let mut exp = ExponentialBackoff::new(start, 0, &mut rng);
        let initial = exp.rto();
        assert!(!exp.can_retry());
        exp.attempt(&mut rng);
        assert_eq!(exp.rto(), initial);
        exp.reset(&mut rng);
        assert!(!exp.can_retry());
        assert_eq!(exp.rto, start);

        let mut exp = ExponentialBackoff::new(start, 2, &mut rng);
        exp.attempt(&mut rng);
        exp.attempt(&mut rng);
        assert!(!exp.can_retry());
        exp.reset(&mut rng);
        assert!(exp.can_retry());
        assert_eq!(exp.left, 2);
        assert_eq!(exp.rto, start);
    }

    #[test]
    fn seeded_jitter_is_repeatable_and_bounded() {
        let mut first_rng = SeededRng::new(Some(42));
        let mut second_rng = SeededRng::new(Some(42));
        let mut first = ExponentialBackoff::new(Duration::from_millis(20), 10, &mut first_rng);
        let mut second = ExponentialBackoff::new(Duration::from_millis(20), 10, &mut second_rng);
        for _ in 0..10 {
            assert_eq!(first.rto(), second.rto());
            assert!((-0.25..0.25).contains(&first.jitter));
            assert!(first.rto() >= first.rto - first.rto / 4);
            assert!(first.rto() <= first.rto + first.rto / 4);
            first.attempt(&mut first_rng);
            second.attempt(&mut second_rng);
        }
    }

    #[test]
    fn attempts() {
        let mut rng = SeededRng::new(Some(42));
        let mut exp = ExponentialBackoff::new(Duration::from_secs(1), 5, &mut rng);

        let n1 = dbg!(exp.rto().as_millis());
        assert_eq!(exp.rto().as_millis(), n1);

        exp.attempt(&mut rng);

        let n2 = dbg!(exp.rto().as_millis());
        assert_eq!(exp.rto().as_millis(), n2);
        assert!(n2 > n1);

        exp.attempt(&mut rng);

        let n3 = dbg!(exp.rto().as_millis());
        assert_eq!(exp.rto().as_millis(), n3);
        assert!(n3 > n2);

        exp.attempt(&mut rng);

        let n4 = dbg!(exp.rto().as_millis());
        assert_eq!(exp.rto().as_millis(), n4);
        assert!(n4 > n3);
        assert!(exp.can_retry());

        exp.attempt(&mut rng);

        let n5 = dbg!(exp.rto().as_millis());
        assert_eq!(exp.rto().as_millis(), n5);
        assert!(n5 > n4);
        assert!(exp.can_retry());

        exp.attempt(&mut rng);

        let n6 = dbg!(exp.rto().as_millis());
        assert_eq!(exp.rto().as_millis(), n6);
        assert!(n6 > n5);
        assert!(!exp.can_retry());

        exp.attempt(&mut rng);

        assert_eq!(exp.rto().as_millis(), n6);
        assert!(!exp.can_retry());
    }
}
