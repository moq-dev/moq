//! Receive, reassembly, and reset: what a server pays to ingest, and to abandon, many streams.

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_quic::{Dir, VarInt};

#[allow(
    dead_code,
    reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Pair, Setup, Sink, ThreadCpu, payload};

/// Stream data per packet, so a stream of `n` packets spans about `n` datagrams.
const PACKET: usize = 1100;

/// Out-of-order ingest: every eighth client datagram is lost, so the server buffers data past a
/// hole in many partial streams at once until each retransmission arrives.
///
/// Only the server's side of the exchange is timed.
fn bench_loss(c: &mut Criterion<ThreadCpu>) {
    let mut group = c.benchmark_group("receive_loss");
    for streams in [1, 16, 256] {
        for packets in [2, 16] {
            let mut pair = None;
            group.throughput(Throughput::Bytes((streams * packets * PACKET) as u64));
            group.bench_function(
                BenchmarkId::new(format!("streams_{streams}"), format!("packets_{packets}")),
                |b| {
                    let pair = pair.get_or_insert_with(|| {
                        let mut pair = Pair::new(Setup::default());
                        pair.lose_every = Some(8);
                        pair
                    });
                    let data = payload(packets * PACKET);
                    b.iter_custom(|iters| {
                        let mut busy = Duration::ZERO;
                        for _ in 0..iters {
                            pair.server.busy = Duration::ZERO;
                            let target = pair.server.received.finished + streams as u64;
                            let conn = pair.client();
                            for _ in 0..streams {
                                let id = conn
                                    .streams()
                                    .open(Dir::Uni, Default::default())
                                    .expect("stream limit");
                                let mut stream = conn.send_stream(id);
                                assert_eq!(stream.write(&data).unwrap(), data.len());
                                stream.finish().unwrap();
                            }
                            pair.drive_until(|pair| pair.server.received.finished >= target);
                            pair.settle();
                            busy += pair.server.busy;
                        }
                        busy
                    });
                },
            );
        }
    }
    group.finish();
}

/// Sender resets: streams carry `bytes` each, of which one burst leaves before every stream is
/// reset, and the exchange runs until the server has seen every reset.
fn bench_reset(c: &mut Criterion<ThreadCpu>) {
    let mut group = c.benchmark_group("receive_reset");
    for streams in [16, 256, 1024] {
        for bytes in [0, 16384] {
            let mut pair = None;
            group.throughput(Throughput::Elements(streams as u64));
            group.bench_function(
                BenchmarkId::new(format!("streams_{streams}"), format!("bytes_{bytes}")),
                |b| {
                    let pair = pair.get_or_insert_with(|| Pair::new(Setup::default()));
                    let data = payload(bytes);
                    b.iter(|| {
                        let target = pair.server.received.reset + streams as u64;
                        let conn = pair.client();
                        let ids: Vec<_> = (0..streams)
                            .map(|_| {
                                let id = conn
                                    .streams()
                                    .open(Dir::Uni, Default::default())
                                    .expect("stream limit");
                                assert_eq!(conn.send_stream(id).write(&data).unwrap(), bytes);
                                id
                            })
                            .collect();
                        pair.step();
                        let conn = pair.client();
                        for id in ids {
                            conn.send_stream(id).reset(VarInt::from_u32(1)).unwrap();
                        }
                        pair.drive_until(|pair| pair.server.received.reset >= target);
                        pair.settle();
                    });
                },
            );
        }
    }
    group.finish();
}

/// In-order ingest read plainly or through leases, so the lease path's cost shows beside the plain
/// read's as the stream count grows.
///
/// Only the server's side of the exchange is timed.
fn bench_lease(c: &mut Criterion<ThreadCpu>) {
    let mut group = c.benchmark_group("receive_lease");
    for (name, sink) in [("read", Sink::Read), ("lease", Sink::Lease(1))] {
        for streams in [1, 16, 256] {
            for packets in [2, 16] {
                let mut pair = None;
                group.throughput(Throughput::Bytes((streams * packets * PACKET) as u64));
                group.bench_function(
                    BenchmarkId::new(
                        format!("{name}/streams_{streams}"),
                        format!("packets_{packets}"),
                    ),
                    |b| {
                        let pair = pair.get_or_insert_with(|| {
                            let mut pair = Pair::new(Setup::default());
                            pair.server.sink = sink;
                            pair
                        });
                        let data = payload(packets * PACKET);
                        b.iter_custom(|iters| {
                            let mut busy = Duration::ZERO;
                            for _ in 0..iters {
                                pair.server.busy = Duration::ZERO;
                                let target = pair.server.received.finished + streams as u64;
                                let conn = pair.client();
                                for _ in 0..streams {
                                    let id = conn
                                        .streams()
                                        .open(Dir::Uni, Default::default())
                                        .expect("stream limit");
                                    let mut stream = conn.send_stream(id);
                                    assert_eq!(stream.write(&data).unwrap(), data.len());
                                    stream.finish().unwrap();
                                }
                                pair.drive_until(|pair| pair.server.received.finished >= target);
                                pair.settle();
                                busy += pair.server.busy;
                            }
                            busy
                        });
                    },
                );
            }
        }
    }
    group.finish();
}

/// A stuck receive window: every stream holds one packet of a unit that never completes, the
/// last one fills the window, and the server evicts one stream to free it.
///
/// The stuck pass ranks every holder, so the cost per stream must stay flat as the count grows.
/// Only the server's side of filling the window and evicting is timed.
fn bench_evict(c: &mut Criterion<ThreadCpu>) {
    let mut group = c.benchmark_group("receive_evict");
    for streams in [16, 64, 256, 1024, 4096] {
        let window = streams * PACKET;
        let mut pair = None;
        group.throughput(Throughput::Elements(streams as u64));
        group.bench_function(BenchmarkId::new("streams", streams), |b| {
            let pair = pair.get_or_insert_with(|| {
                let mut pair = Pair::new(Setup {
                    server_receive_window: VarInt::from_u64(window as u64).unwrap(),
                    ..Default::default()
                });
                // Each reader waits for the whole window, which no single stream fills.
                pair.server.sink = Sink::Lease(window);
                pair
            });
            let data = payload(PACKET);
            b.iter_custom(|iters| {
                let mut busy = Duration::ZERO;
                for _ in 0..iters {
                    pair.server.busy = Duration::ZERO;
                    let target = pair.server.received.evicted + 1;
                    let conn = pair.client();
                    let ids: Vec<_> = (0..streams)
                        .map(|_| {
                            let id = conn
                                .streams()
                                .open(Dir::Uni, Default::default())
                                .expect("stream limit");
                            assert_eq!(conn.send_stream(id).write(&data).unwrap(), PACKET);
                            id
                        })
                        .collect();
                    pair.drive_until(|pair| pair.server.received.evicted >= target);
                    busy += pair.server.busy;

                    // Reset the rest, which returns their credit for the next round.
                    let conn = pair.client();
                    for id in ids {
                        let _ = conn.send_stream(id).reset(VarInt::from_u32(0));
                    }
                    pair.drive_until(|pair| pair.client().streams().send_streams() == 0);
                    pair.settle();
                }
                busy
            });
        });
    }
    group.finish();
}

/// Receiver stops: the server sends STOP_SENDING for every stream as it opens, and the exchange
/// runs until the client has reset and forgotten them all.
fn bench_stop(c: &mut Criterion<ThreadCpu>) {
    let mut group = c.benchmark_group("receive_stop");
    for streams in [16, 256, 1024] {
        for bytes in [1100, 16384] {
            let mut pair = None;
            group.throughput(Throughput::Elements(streams as u64));
            group.bench_function(
                BenchmarkId::new(format!("streams_{streams}"), format!("bytes_{bytes}")),
                |b| {
                    let pair = pair.get_or_insert_with(|| {
                        let mut pair = Pair::new(Setup::default());
                        pair.server.sink = Sink::Stop;
                        pair
                    });
                    let data = payload(bytes);
                    b.iter(|| {
                        let target = pair.server.received.stopped + streams as u64;
                        let conn = pair.client();
                        for _ in 0..streams {
                            let id = conn
                                .streams()
                                .open(Dir::Uni, Default::default())
                                .expect("stream limit");
                            assert_eq!(conn.send_stream(id).write(&data).unwrap(), bytes);
                        }
                        pair.drive_until(|pair| {
                            pair.server.received.stopped >= target
                                && pair.client().streams().send_streams() == 0
                        });
                        pair.settle();
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = common::config();
    targets = bench_loss, bench_lease, bench_evict, bench_reset, bench_stop
}
criterion_main!(benches);
