//! Stream send and scheduling: one client writes many streams to a server that reads them all.
//!
//! Each iteration opens fresh unidirectional streams on a long-lived connection, writes and
//! finishes them, then exchanges packets until the server has read every stream to its end and
//! the path is idle, so the cost covers the scheduler, packetization, the receiver, and the ACKs
//! that free it all.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_quic::Dir;

#[allow(dead_code)]
mod common;

use common::{Pair, Setup, ThreadCpu, payload};

/// Opens `streams` streams across `priorities` levels, writes `writes` writes of `size` bytes to
/// each, and drives until the server has read them all.
fn send(pair: &mut Pair, streams: usize, priorities: usize, writes: usize, size: usize) {
    let data = payload(size);
    let target = pair.server.received.finished + streams as u64;
    let conn = pair.client();
    for index in 0..streams {
        let id = conn.streams().open(Dir::Uni).expect("stream limit");
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

/// Bytes per write against stream count, writing a fixed 64 KiB to each stream.
fn bench_write_size(c: &mut Criterion<ThreadCpu>) {
    const BYTES: usize = 64 << 10;
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
    group.finish();
}

criterion_group! {
    name = benches;
    config = common::config();
    targets = bench_priority, bench_write_size
}
criterion_main!(benches);
