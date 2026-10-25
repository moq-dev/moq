//! Native bandwidth probing ([`Connection::set_probe_target`]): an application-limited media
//! sender raises its estimate toward a target over a seeded bottleneck that steps its rate.
//!
//! Every run sends the same media trace over the same path twice, once probing and once not,
//! so the probe is judged against a baseline that differs only in the target. Each probing run
//! is held to the budgets below, which were set before the probe was tuned. The merge gate runs
//! the short cases; the `#[ignore]`d acceptance suite runs nightly in release with `just rs
//! quic-sim`.

use std::collections::HashMap;

use super::*;
use crate::congestion::{Bbr3Config, CubicConfig};

/// The media's bitrate, 2 Mbit/s, which the probe has to see past
const MEDIA: u64 = 250_000;
/// The rate the session wants, 4 Mbit/s
const TARGET: u64 = 500_000;
const FPS: u64 = 30;
/// Frames per group, each group on its own stream: a keyframe every two seconds
const GOP: u64 = 60;
/// How long a frame may take to arrive and still be played
const DEADLINE: Duration = Duration::from_millis(400);
/// How long after its last frame a group not yet acknowledged is reset as expired
const EXPIRY: Duration = Duration::from_secs(1);

/// Overhead budget: redundant bytes per unique media byte
const OVERHEAD: f64 = 0.10;
/// Deadline budget: frames past [`DEADLINE`] beyond the baseline's, per frame sent
const LATE: f64 = 0.005;
/// Fairness budget: the share of its baseline goodput a competing upload keeps
const FAIRNESS: f64 = 0.9;
/// Rise budget: how soon after the path opens up the estimate reaches the target
const RISE: Duration = Duration::from_secs(5);

/// The bytes of frame `index` of a group whose keyframe is `keyframe` bytes
fn frame_size(keyframe: u64, index: u64) -> u64 {
    let group = MEDIA * GOP / FPS;
    match index {
        0 => keyframe.max(group / GOP),
        _ => (group - keyframe.max(group / GOP)) / (GOP - 1),
    }
}

/// One run's network and workload
#[derive(Clone)]
struct Scenario {
    seed: u64,
    /// One-way propagation delay
    latency: Duration,
    /// The bottleneck's rate from each instant on, the first at the start
    rates: Vec<(Duration, u64)>,
    /// The bottleneck queue in bytes
    buffer: u32,
    /// The probe target; zero for the baseline
    target: u64,
    /// Each group's first frame in bytes; at most the average frame for a constant bitrate
    keyframe: u64,
    duration: Duration,
    /// When the media pauses and resumes
    idle: Option<(Duration, Duration)>,
    /// A bulk upload sharing the bottleneck, under this controller
    competitor: Option<Arc<dyn ControllerFactory + Send + Sync>>,
    /// The receiver's connection window, to limit the sender by credit
    receive_window: Option<u32>,
    /// The ACK frequency the sender requests: ack-eliciting packets per ACK
    ack_threshold: Option<u32>,
    /// Bytes of bulk upload before the media starts, to learn the path's rate first
    warmup: usize,
}

impl Default for Scenario {
    fn default() -> Self {
        Self {
            seed: 1,
            latency: Duration::from_millis(20),
            rates: vec![(Duration::ZERO, 375_000)],
            buffer: 75_000,
            target: TARGET,
            keyframe: 50_000,
            duration: Duration::from_secs(30),
            idle: None,
            competitor: None,
            receive_window: None,
            ack_threshold: None,
            warmup: 0,
        }
    }
}

/// What a run measured
#[derive(Debug, Default)]
struct Outcome {
    /// The sender's state every 100ms from the media's start
    samples: Vec<Sample>,
    frames: u64,
    /// Frames that arrived later than [`DEADLINE`], or never
    late: u64,
    /// Each delivered frame's delay, sorted
    delays: Vec<Duration>,
    /// Media bytes written
    unique: u64,
    /// Bytes of probe packets
    redundant: u64,
    /// UDP payload bytes the media connection sent
    wire: u64,
    queue: QueueStats,
    /// The competing upload's bytes delivered
    competitor: u64,
    /// Groups reset as expired
    expired: u64,
    /// Samples taken while the controller allowed a probe
    probing: u64,
}

/// The sender's state at one instant
#[derive(Debug, Clone, Copy)]
struct Sample {
    /// Since the media's start
    at: Duration,
    /// The bandwidth estimate, in bytes per second
    estimate: u64,
    /// Probe bytes sent so far
    redundant: u64,
}

impl Outcome {
    /// The last sample at or before `at`
    fn at(&self, at: Duration) -> Sample {
        *self
            .samples
            .iter()
            .take_while(|sample| sample.at <= at)
            .last()
            .expect("sampled")
    }

    /// The first instant in `range` the estimate meets `pred`
    fn first(&self, range: ops::Range<Duration>, pred: impl Fn(u64) -> bool) -> Option<Duration> {
        self.samples
            .iter()
            .find(|sample| range.contains(&sample.at) && pred(sample.estimate))
            .map(|sample| sample.at)
    }

    /// Asserts the estimate reaches the target within [`RISE`] of `from`, returning how soon
    fn rises(&self, from: Duration) -> Duration {
        let reached = self.first(from..from + RISE, |estimate| estimate >= TARGET);
        let reached = reached.unwrap_or_else(|| {
            panic!(
                "estimate {} B/s short of {TARGET} B/s {RISE:?} after {from:?}",
                self.at(from + RISE).estimate
            )
        });
        reached - from
    }

    fn percentile(&self, p: f64) -> Duration {
        self.delays
            .get(((self.delays.len() as f64 * p) as usize).min(self.delays.len().saturating_sub(1)))
            .copied()
            .unwrap_or_default()
    }

    fn report(&self, label: &str) {
        let estimates = self
            .samples
            .iter()
            .step_by(10)
            .map(|sample| {
                let mbps = sample.estimate as f64 * 8.0 / 1e6;
                format!("{}s:{mbps:.2}", sample.at.as_secs())
            })
            .collect::<Vec<_>>()
            .join(" ");
        info!(
            label,
            frames = self.frames,
            late = self.late,
            p50 = ?self.percentile(0.5),
            p99 = ?self.percentile(0.99),
            max = ?self.delays.last(),
            unique = self.unique,
            redundant = self.redundant,
            overhead = self.redundant as f64 / self.unique.max(1) as f64,
            wire = self.wire,
            mean_queue = ?self.queue.mean_delay(),
            max_queue = ?self.queue.max_delay,
            dropped = self.queue.dropped,
            competitor = self.competitor,
            expired = self.expired,
            probing = self.probing,
            "Mbit/s {estimates}"
        );
    }
}

/// A group the sender has opened
struct Group {
    id: StreamId,
    /// When its last frame was written, once it was
    ended: Option<Instant>,
    /// When it was reset as expired
    reset: Option<Instant>,
    /// When each of its frames was written
    written: Vec<Instant>,
    /// Frame bytes written that the stream had no credit for yet
    backlog: Vec<u8>,
    finished: bool,
}

/// A group the receiver is reading
#[derive(Default)]
struct Reading {
    read: u64,
    /// The next frame to complete
    frame: u64,
    done: bool,
}

fn run(scenario: &Scenario) -> Outcome {
    entropy::reseed(scenario.seed);
    let mut server_transport = TransportConfig::default();
    if let Some(window) = scenario.receive_window {
        server_transport
            .receive_window(window.into())
            .stream_receive_window(window.into());
    }
    server_transport.max_concurrent_uni_streams(1000u32.into());
    let mut server_config = server_config();
    server_config.transport = Arc::new(server_transport);
    let mut pair = Pair::new(Default::default(), server_config);
    pair.latency = scenario.latency;

    let mut transport = TransportConfig::default();
    if let Some(threshold) = scenario.ack_threshold {
        let mut ack = AckFrequencyConfig::default();
        ack.ack_eliciting_threshold(threshold.into())
            .max_ack_delay(Some(Duration::from_millis(25)));
        transport.ack_frequency_config(Some(ack));
    }
    let mut config = client_config();
    config.transport = Arc::new(transport);
    let (client_ch, server_ch) = pair.connect_with(config);
    let competitor = scenario.competitor.as_ref().map(|factory| {
        let mut transport = TransportConfig::default();
        transport.congestion_controller_factory(factory.clone());
        let mut config = client_config();
        config.transport = Arc::new(transport);
        pair.connect_with(config)
    });

    let (_, first) = scenario.rates[0];
    pair.to_server.bottleneck = Some(Bottleneck::new(first, scenario.buffer, false));
    pair.client_conn_mut(client_ch)
        .set_probe_target(scenario.target);

    if scenario.warmup > 0 {
        super::upload(&mut pair, client_ch, server_ch, scenario.warmup, |_| {});
    }

    let start = pair.time;
    let end = start + scenario.duration;
    let mut outcome = Outcome::default();
    let mut groups: Vec<Group> = Vec::new();
    let mut readings: HashMap<StreamId, Reading> = HashMap::new();
    let mut next_frame = 0u64;
    let mut next_sample = start;
    let mut next_rate = 1;
    let mut competing = competitor.map(|(client, server)| {
        let id = pair
            .client_streams(client)
            .open(Dir::Uni, Default::default())
            .unwrap();
        (client, server, id, None::<StreamId>)
    });

    loop {
        let now = pair.time;
        let elapsed = now - start;
        if next_rate < scenario.rates.len() && elapsed >= scenario.rates[next_rate].0 {
            let rate = scenario.rates[next_rate].1;
            pair.to_server.bottleneck.as_mut().unwrap().set_rate(rate);
            next_rate += 1;
        }

        // Write every frame that is due
        let frame_at = |n: u64| start + Duration::from_nanos(n * 1_000_000_000 / FPS);
        while frame_at(next_frame) <= now && frame_at(next_frame) < end {
            let at = frame_at(next_frame) - start;
            let paused = scenario
                .idle
                .is_some_and(|(from, until)| at >= from && at < until);
            if !paused {
                write_frame(
                    &mut pair,
                    client_ch,
                    &mut groups,
                    scenario.keyframe,
                    next_frame,
                    now,
                );
                outcome.frames += 1;
                outcome.unique += frame_size(scenario.keyframe, next_frame % GOP);
            }
            next_frame += 1;
        }
        expire(&mut pair, client_ch, &mut groups, now, &mut outcome);
        flush(&mut pair, client_ch, &mut groups);

        if let Some((client, _, id, _)) = &competing {
            let mut stream = pair.client_send(*client, *id);
            while stream.write(&[0; 16_384]).is_ok() {}
        }

        pair.drive_client();
        pair.drive_server();

        receive(
            &mut pair,
            server_ch,
            scenario.keyframe,
            &mut readings,
            &groups,
            now,
            &mut outcome,
        );
        if let Some((_, server, _, recv)) = &mut competing {
            if recv.is_none() {
                *recv = pair.server_streams(*server).accept(Dir::Uni);
            }
            if let Some(id) = *recv {
                let mut stream = pair.server_recv(*server, id);
                let mut chunks = stream.read(false).unwrap();
                while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
                    outcome.competitor += chunk.bytes.len() as u64;
                }
                let _ = chunks.finalize();
            }
        }

        // No probe resends a group's data once it is reset
        let conn = pair.client_conn_mut(client_ch);
        for (sent, id) in conn.bandwidth_probes_in_flight() {
            let group = groups.iter().find(|g| g.id == id).unwrap();
            assert!(
                group.reset.is_none_or(|reset| sent < reset),
                "resent stream {id} at {sent:?} after its reset"
            );
        }

        if now >= next_sample {
            let estimate = conn.stats().path.bandwidth_estimate.unwrap_or(0);
            outcome.probing += conn.congestion_state().metrics().probe_inflight.is_some() as u64;
            outcome.samples.push(Sample {
                at: now - start,
                estimate,
                redundant: conn.stats().path.sent_bandwidth_probe_bytes,
            });
            next_sample += Duration::from_millis(100);
        }
        if now >= end {
            break;
        }

        let wakeups = [
            pair.client.next_wakeup(),
            pair.server.next_wakeup(),
            Some(frame_at(next_frame)),
            Some(next_sample),
            scenario.rates.get(next_rate).map(|&(at, _)| start + at),
        ];
        let next = wakeups.into_iter().flatten().min().unwrap();
        pair.time = pair.time.max(next);
    }

    // Frames still missing count as late
    for group in &groups {
        let frames = group.written.len() as u64;
        let completed = readings.get(&group.id).map_or(0, |r| r.frame);
        outcome.late += frames - completed.min(frames);
    }
    outcome.delays.sort();
    let stats = pair.client_conn_mut(client_ch).stats();
    outcome.redundant = stats.path.sent_bandwidth_probe_bytes;
    outcome.wire = stats.udp_tx.bytes;
    outcome.queue = pair.to_server.bottleneck.take().unwrap().stats;
    outcome
}

/// Write frame `n` of the trace, opening its group's stream on a keyframe
fn write_frame(
    pair: &mut Pair,
    ch: ConnectionHandle,
    groups: &mut Vec<Group>,
    keyframe: u64,
    n: u64,
    now: Instant,
) {
    let index = n % GOP;
    if index == 0 || groups.is_empty() {
        if let Some(group) = groups.last_mut() {
            group.ended.get_or_insert(now);
        }
        let sequence = groups.len() as i64;
        let id = pair
            .client_streams(ch)
            .open(
                Dir::Uni,
                SendOptions {
                    group: None,
                    priority: sequence,
                },
            )
            .expect("stream limit");
        groups.push(Group {
            id,
            ended: None,
            reset: None,
            written: Vec::new(),
            backlog: Vec::new(),
            finished: false,
        });
    }
    let group = groups.last_mut().unwrap();
    if group.reset.is_none() {
        let data = [(n % 251) as u8].repeat(frame_size(keyframe, index) as usize);
        group.backlog.extend(data);
        group.written.push(now);
    }
}

/// Write what credit allows of each group's backlog, finishing every ended group once written
fn flush(pair: &mut Pair, ch: ConnectionHandle, groups: &mut [Group]) {
    for group in groups
        .iter_mut()
        .filter(|g| g.reset.is_none() && !g.finished)
    {
        let mut stream = pair.client_send(ch, group.id);
        while !group.backlog.is_empty() {
            match stream.write(&group.backlog) {
                Ok(n) => drop(group.backlog.drain(..n)),
                Err(WriteError::Blocked) => return,
                Err(e) => panic!("write: {e}"),
            }
        }
        if group.ended.is_some() {
            stream.finish().unwrap();
            group.finished = true;
        }
    }
}

/// Reset every group that ended [`EXPIRY`] ago and is not yet acknowledged
fn expire(
    pair: &mut Pair,
    ch: ConnectionHandle,
    groups: &mut [Group],
    now: Instant,
    outcome: &mut Outcome,
) {
    for group in groups.iter_mut() {
        let Some(ended) = group.ended else { continue };
        if group.reset.is_some() || now < ended + EXPIRY {
            continue;
        }
        let mut stream = pair.client_send(ch, group.id);
        let offset = stream.offset();
        let Ok(false) = stream.acked(offset) else {
            group.reset = Some(ended);
            continue;
        };
        stream.reset(VarInt(0)).unwrap();
        group.reset = Some(now);
        group.backlog.clear();
        outcome.expired += 1;
    }
}

/// Read every group that has arrived, completing frames in order
fn receive(
    pair: &mut Pair,
    ch: ConnectionHandle,
    keyframe: u64,
    readings: &mut HashMap<StreamId, Reading>,
    groups: &[Group],
    now: Instant,
    outcome: &mut Outcome,
) {
    let conn = pair.server_conn_mut(ch);
    while let Some(id) = conn.streams().accept(Dir::Uni) {
        readings.insert(id, Reading::default());
    }
    for (&id, reading) in readings.iter_mut() {
        if reading.done {
            continue;
        }
        let mut stream = conn.recv_stream(id);
        let Ok(mut chunks) = stream.read(true) else {
            reading.done = true;
            continue;
        };
        loop {
            match chunks.next(usize::MAX) {
                Ok(Some(chunk)) => reading.read += chunk.bytes.len() as u64,
                Ok(None) => {
                    reading.done = true;
                    break;
                }
                Err(ReadError::Blocked) => break,
                Err(_) => {
                    reading.done = true;
                    break;
                }
            }
        }
        let _ = chunks.finalize();

        // The sender opens each group's stream in order, after any it opened before the media
        let frames = &groups[(id.index() - groups[0].id.index()) as usize].written;
        let mut end: u64 = (0..reading.frame).map(|i| frame_size(keyframe, i)).sum();
        while (reading.frame as usize) < frames.len() {
            end += frame_size(keyframe, reading.frame);
            if reading.read < end {
                break;
            }
            let delay = now - frames[reading.frame as usize];
            outcome.late += (delay > DEADLINE) as u64;
            outcome.delays.push(delay);
            reading.frame += 1;
        }
    }
}

/// Runs `scenario` probing and as the baseline, reporting both and holding the probing run to
/// every budget
fn compare(label: &str, scenario: Scenario) -> (Outcome, Outcome) {
    let rtt = 2 * scenario.latency;
    let probing = run(&scenario);
    probing.report(&format!("{label} probing"));
    let baseline = run(&Scenario {
        target: 0,
        ..scenario
    });
    baseline.report(&format!("{label} baseline"));
    assert_eq!(baseline.redundant, 0);

    let overhead = probing.redundant as f64 / probing.unique as f64;
    assert!(
        overhead <= OVERHEAD,
        "{label}: overhead {overhead:.3} over {OVERHEAD}"
    );
    let late = baseline.late + (probing.frames as f64 * LATE) as u64;
    assert!(
        probing.late <= late,
        "{label}: {} late frames, {} for the baseline",
        probing.late,
        baseline.late
    );
    let p99 = probing.percentile(0.99);
    assert!(
        p99 <= baseline.percentile(0.99) + rtt,
        "{label}: p99 {p99:?}, {:?} for the baseline",
        baseline.percentile(0.99)
    );
    assert!(
        probing.competitor as f64 >= baseline.competitor as f64 * FAIRNESS,
        "{label}: competitor got {} bytes, {} beside the baseline",
        probing.competitor,
        baseline.competitor
    );
    (probing, baseline)
}

/// Reports how long after `from` the estimate fell to within the ProbeBW_UP gain of `rate`
fn report_fall(label: &str, outcome: &Outcome, from: Duration, rate: u64) {
    let fell = outcome.first(from..Duration::MAX, |estimate| estimate <= rate * 5 / 4);
    info!(label, age = ?fell.map(|at| at - from), "estimate age after the path fell");
}

/// A 3 Mbit/s path, below the target, that opens up to 12 Mbit/s at 20s and falls back at 40s
fn steps() -> Scenario {
    Scenario {
        rates: vec![
            (Duration::ZERO, 375_000),
            (Duration::from_secs(20), 1_500_000),
            (Duration::from_secs(40), 375_000),
        ],
        duration: Duration::from_secs(60),
        ..Scenario::default()
    }
}

/// A constant bitrate never fills its estimate, so only the probe finds the path opened up: at
/// this 100ms round trip the baseline's estimate stays at the first path's 3 Mbit/s
#[test]
fn rises_to_target() {
    let _guard = subscribe();
    let scenario = Scenario {
        latency: Duration::from_millis(50),
        buffer: 170_000,
        keyframe: 0,
        rates: vec![
            (Duration::ZERO, 375_000),
            (Duration::from_secs(5), 1_500_000),
        ],
        duration: Duration::from_secs(15),
        ..Scenario::default()
    };
    let (probing, _) = compare("rises", scenario);
    probing.rises(Duration::from_secs(5));
}

/// Below the media's bitrate groups expire, and none is resent after its reset
///
/// BBR keeps a queue a second deep from filling, so a queue of one BDP checks the overflow.
#[test]
fn congested() {
    let _guard = subscribe();
    for buffer in [187_500, 7_500] {
        let scenario = Scenario {
            rates: vec![(Duration::ZERO, 187_500)],
            buffer,
            duration: Duration::from_secs(15),
            ..Scenario::default()
        };
        let (probing, baseline) = compare(&format!("congested buffer {buffer}"), scenario);
        for outcome in [&probing, &baseline] {
            assert!(outcome.expired > 0, "no group expired");
            assert!(
                buffer > 7_500 || outcome.queue.dropped > 0,
                "the queue never overflowed"
            );
        }
    }
}

/// A write that fits one packet still gets resent: the probe looks for in-flight data while that
/// packet is being built, before it is tracked, and the flight drains between writes
#[test]
fn single_packet_writes() {
    let _guard = subscribe();
    let mut pair = Pair::default();
    pair.latency = Duration::from_millis(20);
    let (client_ch, _) = pair.connect();
    pair.client_conn_mut(client_ch).set_probe_target(TARGET);
    let id = pair
        .client_streams(client_ch)
        .open(Dir::Uni, Default::default())
        .unwrap();

    let end = pair.time + Duration::from_secs(1);
    let mut next_write = pair.time;
    while pair.time < end {
        if pair.time >= next_write {
            pair.client_send(client_ch, id).write(&[0; 100]).unwrap();
            next_write += Duration::from_millis(200);
        }
        pair.drive_client();
        pair.drive_server();
        let wakeups = [
            pair.client.next_wakeup(),
            pair.server.next_wakeup(),
            Some(next_write),
        ];
        pair.time = pair.time.max(wakeups.into_iter().flatten().min().unwrap());
    }
    assert!(pair.client_conn_mut(client_ch).stats().path.sent_bandwidth_probes > 0);
}

#[test]
#[ignore = "nightly: just rs quic-sim"]
fn steps_keyframes() {
    let _guard = subscribe();
    let (probing, baseline) = compare("steps keyframes", steps());
    probing.rises(Duration::from_secs(20));
    report_fall(
        "steps keyframes probing",
        &probing,
        Duration::from_secs(40),
        375_000,
    );
    report_fall(
        "steps keyframes baseline",
        &baseline,
        Duration::from_secs(40),
        375_000,
    );
}

#[test]
#[ignore = "nightly: just rs quic-sim"]
fn steps_constant() {
    let _guard = subscribe();
    let (probing, baseline) = compare(
        "steps constant",
        Scenario {
            keyframe: 0,
            ..steps()
        },
    );
    probing.rises(Duration::from_secs(20));
    report_fall(
        "steps constant probing",
        &probing,
        Duration::from_secs(40),
        375_000,
    );
    report_fall(
        "steps constant baseline",
        &baseline,
        Duration::from_secs(40),
        375_000,
    );
}

/// Nothing is in flight while the media pauses, so nothing is resent
#[test]
#[ignore = "nightly: just rs quic-sim"]
fn idle_restart() {
    let _guard = subscribe();
    let idle = (Duration::from_secs(25), Duration::from_secs(30));
    let scenario = Scenario {
        keyframe: 0,
        idle: Some(idle),
        ..steps()
    };
    let (probing, _) = compare("idle", scenario);
    probing.rises(Duration::from_secs(20));
    // The last frames drain within a second
    let drained = probing.at(idle.0 + Duration::from_secs(1)).redundant;
    assert_eq!(probing.at(idle.1).redundant, drained, "probed while idle");
}

/// An estimate learned before the path fell holds above the target, so the probe stays idle
/// until the controller lowers it
#[test]
#[ignore = "nightly: just rs quic-sim"]
fn stale_estimate() {
    let _guard = subscribe();
    let scenario = Scenario {
        rates: vec![
            (Duration::ZERO, 1_500_000),
            (Duration::from_nanos(1), 375_000),
        ],
        warmup: 3_000_000,
        ..Scenario::default()
    };
    let (probing, baseline) = compare("stale", scenario);
    report_fall("stale probing", &probing, Duration::ZERO, 375_000);
    report_fall("stale baseline", &baseline, Duration::ZERO, 375_000);
}

/// Resent data needs no new credit, so a receive window that limits the media does not limit
/// the probe
#[test]
#[ignore = "nightly: just rs quic-sim"]
fn credit_limited() {
    let _guard = subscribe();
    let scenario = Scenario {
        receive_window: Some(32_000),
        ..steps()
    };
    let (probing, _) = compare("credit", scenario);
    probing.rises(Duration::from_secs(20));
}

#[test]
#[ignore = "nightly: just rs quic-sim"]
fn compressed_acks() {
    let _guard = subscribe();
    let scenario = Scenario {
        ack_threshold: Some(10),
        ..steps()
    };
    let (probing, _) = compare("acks", scenario);
    probing.rises(Duration::from_secs(20));
}

/// A bulk upload sharing a 6 Mbit/s path keeps its goodput while the media probes beside it
/// for more than its fair share
fn competing(label: &str, factory: Arc<dyn ControllerFactory + Send + Sync>) {
    let _guard = subscribe();
    let scenario = Scenario {
        rates: vec![(Duration::ZERO, 750_000)],
        buffer: 150_000,
        competitor: Some(factory),
        ..Scenario::default()
    };
    compare(label, scenario);
}

#[test]
#[ignore = "nightly: just rs quic-sim"]
fn competing_cubic() {
    competing("cubic", Arc::new(CubicConfig::default()));
}

#[test]
#[ignore = "nightly: just rs quic-sim"]
fn competing_bbr() {
    competing("bbr", Arc::new(Bbr3Config::default()));
}

/// A target the path can never carry keeps the probe trying, within the overhead budget
#[test]
#[ignore = "nightly: just rs quic-sim"]
fn unreachable_target() {
    let _guard = subscribe();
    let scenario = Scenario {
        target: 12_500_000,
        ..Scenario::default()
    };
    compare("unreachable", scenario);
}

/// From a queue of one BDP at 3 Mbit/s and 40ms to a bufferbloated 64
#[test]
#[ignore = "nightly: just rs quic-sim"]
fn queue_depths() {
    let _guard = subscribe();
    for buffer in [15_000, 60_000, 240_000, 960_000] {
        let (probing, _) = compare(&format!("buffer {buffer}"), Scenario { buffer, ..steps() });
        probing.rises(Duration::from_secs(20));
    }
}

/// Round trips from 10 to 200ms, each behind a queue of four BDPs at 3 Mbit/s
fn round_trip(latency: u64) {
    let _guard = subscribe();
    for keyframe in [0, 50_000] {
        let scenario = Scenario {
            latency: Duration::from_millis(latency),
            keyframe,
            buffer: 375_000 * latency as u32 / 1000 * 8 + 20_000,
            ..steps()
        };
        let label = format!("rtt {}ms keyframe {keyframe}", 2 * latency);
        let (probing, _) = compare(&label, scenario);
        probing.rises(Duration::from_secs(20));
    }
}

#[test]
#[ignore = "nightly: just rs quic-sim"]
fn rtt_10ms() {
    round_trip(5);
}

#[test]
#[ignore = "nightly: just rs quic-sim"]
fn rtt_100ms() {
    round_trip(50);
}

#[test]
#[ignore = "nightly: just rs quic-sim"]
fn rtt_200ms() {
    round_trip(100);
}
