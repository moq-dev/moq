//! Stream send and scheduling: one client writes many streams to a server that reads them all.
//!
//! Throughput cases open fresh unidirectional streams on a long-lived connection, write and
//! finish them, then exchange packets until the server has read them and the path is idle.
//! The zero-credit case isolates blocked writes and resets across live-writer counts, with
//! connection setup and dropping the remaining state outside the CPU measurement.

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_quic::Dir;

#[allow(
    dead_code,
    reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Pair, Setup, ThreadCpu, payload};

/// How often a `holes` case loses a client datagram: often enough that most windows hold a hole.
const LOSE_EVERY: u64 = 16;

/// Opens `streams` streams across `priorities` levels, writes `writes` writes of `size` bytes to
/// each, and drives until the server has read them all.
fn send(pair: &mut Pair, streams: usize, priorities: usize, writes: usize, size: usize) {
    let data = payload(size);
    let target = pair.server.received.finished + streams as u64;
    let conn = pair.client();
    for index in 0..streams {
        let id = conn
            .streams()
            .open(Dir::Uni, Default::default())
            .expect("stream limit");
        let mut stream = conn.send_stream(id);
        // `as _` follows the priority type, which a scheduler rewrite widens.
        stream.set_priority((index % priorities) as _).unwrap();
        for _ in 0..writes {
            assert_eq!(stream.write(&data).unwrap(), size, "write was cut short");
        }
        stream.finish().unwrap();
    }
    pair.drive_until(|pair| pair.server.received.finished >= target);
    pair.settle();
}

/// Stream count against priority levels, with a few packets per stream so they interleave.
fn bench_priority(c: &mut Criterion<ThreadCpu>) {
    const BYTES: usize = 4096;
    let mut group = c.benchmark_group("send_priority");
    for streams in [1, 16, 256, 1024] {
        for priorities in [1, 8, 64] {
            if priorities > streams {
                continue;
            }
            let mut pair = None;
            group.throughput(Throughput::Bytes((streams * BYTES) as u64));
            group.bench_function(
                BenchmarkId::new(
                    format!("streams_{streams}"),
                    format!("priorities_{priorities}"),
                ),
                |b| {
                    let pair = pair.get_or_insert_with(|| Pair::new(Setup::default()));
                    b.iter(|| send(pair, streams, priorities, 1, BYTES))
                },
            );
        }
    }
    group.finish();
}

/// Writes `total` bytes to one stream in `size`-byte writes, exchanging packets whenever the
/// client's send window is full, and drives until the server has read them all.
fn send_windowed(pair: &mut Pair, total: usize, size: usize) {
    let data = payload(size);
    let target = pair.server.received.finished + 1;
    let id = pair
        .client()
        .streams()
        .open(Dir::Uni, Default::default())
        .expect("stream limit");
    let mut written = 0;
    while written < total {
        if pair.client().send_stream(id).write_limit() == 0 {
            pair.drive_until(|pair| pair.client().send_stream(id).write_limit() > 0);
        }
        // A write the window cuts short resumes mid-chunk, so every chunk boundary stays put.
        let start = written % size;
        written += pair.client().send_stream(id).write(&data[start..]).unwrap();
    }
    pair.client().send_stream(id).finish().unwrap();
    pair.drive_until(|pair| pair.server.received.finished >= target);
    pair.settle();
}

/// Bytes per write against stream count, writing a fixed 64 KiB to each stream.
///
/// The `window` cases instead write a fixed 256 KiB to one stream through a send window, so the
/// data retained until acknowledged grows with the window while the total stays put. The
/// `holes` cases, at the largest window only to keep the nightly short, lose a datagram now and
/// then, so a hole keeps everything after it retained and its retransmission reads old offsets.
fn bench_write_size(c: &mut Criterion<ThreadCpu>) {
    const BYTES: usize = 64 << 10;
    const TOTAL: usize = 256 << 10;
    let mut group = c.benchmark_group("send_write_size");
    for streams in [1, 64] {
        for size in [64, 1024, 16384] {
            let mut pair = None;
            group.throughput(Throughput::Bytes((streams * BYTES) as u64));
            group.bench_function(
                BenchmarkId::new(format!("streams_{streams}"), format!("bytes_{size}")),
                |b| {
                    let pair = pair.get_or_insert_with(|| Pair::new(Setup::default()));
                    b.iter(|| send(pair, streams, 1, BYTES / size, size))
                },
            );
        }
    }
    let windows = [
        (16 << 10, false),
        (64 << 10, false),
        (TOTAL, false),
        (TOTAL, true),
    ];
    for (window, holes) in windows {
        for size in [64, 1024, 16384] {
            let mut pair = None;
            let name = match holes {
                false => format!("window_{}k", window >> 10),
                true => format!("window_{}k_holes", window >> 10),
            };
            group.throughput(Throughput::Bytes(TOTAL as u64));
            group.bench_function(BenchmarkId::new(name, format!("bytes_{size}")), |b| {
                let pair = pair.get_or_insert_with(|| {
                    let mut pair = Pair::new(Setup {
                        client_send_window: window as u64,
                        ..Setup::default()
                    });
                    pair.lose_every = holes.then_some(LOSE_EVERY);
                    pair
                });
                b.iter(|| send_windowed(pair, TOTAL, size))
            });
        }
    }
    group.finish();
}

/// Keep `live` writers blocked while `churn` others block and reset, with MAX_DATA held at zero.
/// A fresh connection per sample bounds even the unfixed implementation's retained history.
fn bench_connection_blocked(c: &mut Criterion<ThreadCpu>) {
    let mut group = c.benchmark_group("connection_blocked");
    for live in [1, 64, 1024] {
        for churn in [16, 256] {
            group.throughput(Throughput::Elements(churn));
            group.bench_function(
                BenchmarkId::new(format!("live_{live}"), format!("churn_{churn}")),
                |b| {
                    b.iter_batched(
                        || {
                            let mut pair = Pair::new(Setup {
                                server_receive_window: 0u32.into(),
                                ..Setup::default()
                            });
                            for _ in 0..live {
                                let id = pair
                                    .client()
                                    .streams()
                                    .open(Dir::Uni, Default::default())
                                    .unwrap();
                                assert_eq!(
                                    pair.client().send_stream(id).write(b"x"),
                                    Err(moq_quic::WriteError::Blocked)
                                );
                            }
                            pair
                        },
                        |mut pair| {
                            for _ in 0..churn {
                                let id = pair
                                    .client()
                                    .streams()
                                    .open(Dir::Uni, Default::default())
                                    .unwrap();
                                let mut stream = pair.client().send_stream(id);
                                assert_eq!(stream.write(b"x"), Err(moq_quic::WriteError::Blocked));
                                stream.reset(0u32.into()).unwrap();
                            }
                            // The peer never grants credit. Packet exchange and dropping the live
                            // table are outside the measured insert/write/reset path.
                            pair
                        },
                        BatchSize::PerIteration,
                    )
                },
            );
        }
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = common::config();
    targets = bench_priority, bench_write_size, bench_connection_blocked
}
criterion_main!(benches);
