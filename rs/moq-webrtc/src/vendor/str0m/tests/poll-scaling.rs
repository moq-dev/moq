//! How much does one `poll_output()` cost, as a function of the number of
//! m-lines on the connection?
//!
//! This is the shape that matters for a server. A 1:1 call has 2 m-lines and the
//! cost is invisible. An SFU client in an N-person room has ~N m-lines (one per
//! other participant), and `poll_output` is called thousands of times per second
//! per connection — so any per-call linear scan over medias/streams is paid
//! O(N) times, O(N^2) per room.
//!
//! The number reported is the cost of a poll that finds NOTHING to do (returns
//! `Output::Timeout`). In a real SFU that is the majority of calls: every drain
//! ends with one, by construction.
//!
//! Ignored by default, since it is a measurement rather than an assertion and it
//! takes a while. Run with:
//!
//!     cargo test --release --test poll-scaling -- --ignored --nocapture
//!
//! `--release` is essential; a debug build hides the effect entirely.
//!
//! Methodology note, because it cost us several wrong conclusions. Report the
//! MINIMUM of N trials, never the mean: CPU benchmark noise is additive, so the
//! fastest trial is the closest to the true cost. And never compare two numbers
//! measured minutes apart, because the machine drifts (thermal throttling, power
//! source). To compare two versions, prebuild both test binaries, run them back to
//! back alternating the order, and pair the results.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::media::{Direction, MediaKind, Mid};
use str0m::{Output, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, negotiate, progress};

/// m-line counts to sweep. 2 = a 1:1 call. 30 = a mid-size conference.
const MLINES: &[usize] = &[2, 5, 10, 20, 30, 50];

/// Polls timed per trial.
const ITERATIONS: usize = 50_000;

/// Trials per data point. We report the MINIMUM: a CPU benchmark's noise is
/// almost entirely additive (scheduling, interrupts, frequency dips), so the
/// fastest trial is the closest to the true cost. Reporting the mean instead
/// makes the numbers swing ~20% run to run and the comparison meaningless.
const TRIALS: usize = 7;

#[test]
#[ignore = "measurement, not an assertion; run explicitly with --ignored"]
fn poll_output_cost_by_mline_count() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    println!();
    println!("cost of ONE poll_output() that returns Timeout (i.e. finds nothing to do)");
    println!();
    println!(
        "{:>8}  {:>12}  {:>12}  {:>10}",
        "m-lines", "ns/poll", "vs 2 m-lines", "ns/m-line"
    );
    println!("{:->8}  {:->12}  {:->12}  {:->10}", "", "", "", "");

    let mut baseline: Option<f64> = None;

    for &n in MLINES {
        let ns = measure(n)?;

        let base = *baseline.get_or_insert(ns);
        println!(
            "{:>8}  {:>12.0}  {:>11.1}x  {:>10.1}",
            n,
            ns,
            ns / base,
            ns / n as f64
        );
    }

    println!();
    println!("A flat 'vs 2 m-lines' column means poll_output is O(1) in the number of");
    println!("m-lines. A column that tracks the m-line count means it is O(N), and an");
    println!("SFU pays that N times per iteration.");
    println!();

    Ok(())
}

/// Build a connection with `n` send-only m-lines (the SFU-to-viewer direction),
/// get media flowing on each so the StreamTx actually exist, then time a
/// `poll_output()` that has nothing left to produce.
fn measure(n: usize) -> Result<f64, RtcError> {
    let mut l = TestRtc::new(Peer::Left); // the "SFU"
    let mut r = TestRtc::new(Peer::Right); // the viewer

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // One m-line per remote participant the viewer is subscribed to.
    let mids: Vec<Mid> = negotiate(&mut l, &mut r, |change| {
        (0..n)
            .map(|_| change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None))
            .collect()
    });

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    // Write one packet on every m-line, so each has a live StreamTx. Without this
    // the streams do not exist and the scan has nothing to scan.
    let params = l.params_vp8();
    let pt = params.pt();
    for (i, mid) in mids.iter().enumerate() {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(*mid)
            .unwrap()
            .write(pt, wallclock, time, vec![1_u8, 2, 3, (i % 250) as u8])?;
        progress(&mut l, &mut r)?;
    }

    // Drain everything so the next poll is a pure "nothing to do" poll.
    loop {
        match l.rtc.poll_output()? {
            Output::Timeout(_) => break,
            Output::Transmit(_) | Output::Event(_) => continue,
        }
    }

    // Warm up: caches, branch predictors, CPU frequency.
    for _ in 0..ITERATIONS {
        std::hint::black_box(l.rtc.poll_output()?);
    }

    // Time the empty poll. It is idempotent — it keeps returning Timeout — so we
    // can call it repeatedly without changing the connection's state.
    let mut best = f64::MAX;
    for _ in 0..TRIALS {
        let t0 = Instant::now();
        for _ in 0..ITERATIONS {
            let out = l.rtc.poll_output()?;
            debug_assert!(matches!(out, Output::Timeout(_)));
            std::hint::black_box(&out);
        }
        let elapsed: Duration = t0.elapsed();
        let ns = elapsed.as_nanos() as f64 / ITERATIONS as f64;
        if ns < best {
            best = ns;
        }
    }

    Ok(best)
}
