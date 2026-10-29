import Darwin
import Foundation
import XCTest
import Moq

private struct Case {
    let size: Int
    let publishers: Int
    let subscribers: Int
    var frames: Int { min(max((8 << 20) / size, 1), 64) }
    var delivered: Int { publishers * subscribers * frames * size }
    var name: String { "size=\(size)/pub=\(publishers)/sub=\(subscribers)" }
}

private struct Result: Encodable {
    let name: String
    let unit: String
    let value: Double
}

private func cpu() throws -> Double {
    var usage = rusage()
    guard getrusage(RUSAGE_SELF, &usage) == 0 else {
        throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
    }
    return Double(usage.ru_utime.tv_sec + usage.ru_stime.tv_sec)
        + Double(usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) / 1e6
}

/// The Swift runner of the shared binding benchmark, opt-in through MOQ_BENCH_DIR.
final class FfiBench: XCTestCase {
    func testBenchmark() async throws {
        guard let dir = ProcessInfo.processInfo.environment["MOQ_BENCH_DIR"] else { return }
        let setting = ProcessInfo.processInfo.environment["MOQ_BENCH_SECONDS"] ?? "1"
        let budget = try XCTUnwrap(Double(setting))
        guard budget.isFinite && budget > 0 else {
            throw NSError(domain: "MOQ_BENCH_SECONDS must be positive and finite", code: 1)
        }
        var results = [Result]()
        let cases = [(100, 1, 1), (1024, 1, 1), (65536, 1, 1), (1048576, 1, 1),
                     (1024, 1, 4), (1024, 4, 1), (1024, 4, 4)]
        for (size, publishers, subscribers) in cases {
            results += try await throughput(Case(size: size, publishers: publishers, subscribers: subscribers), budget)
        }
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "calls")
        let producer = try track.appendGroup()
        let group = try producer.consume()
        let consumer = try track.consume()
        func call(_ name: String, _ fn: () throws -> Void) rethrows -> Result {
            let start = ProcessInfo.processInfo.systemUptime
            var calls = 0
            repeat {
                for _ in 0..<1000 { try fn() }
                calls += 1000
            } while ProcessInfo.processInfo.systemUptime - start < budget
            return Result(name: "call/\(name)", unit: "ns",
                          value: (ProcessInfo.processInfo.systemUptime - start) * 1e9 / Double(calls))
        }
        results.append(call("sequence") { _ = group.sequence })
        results.append(try call("info") { _ = consumer.info() })
        try broadcast.close()
        let directory = URL(fileURLWithPath: dir, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try JSONEncoder().encode(results).write(to: directory.appendingPathComponent("swift.json"))
    }

    private func throughput(_ c: Case, _ budget: Double) async throws -> [Result] {
        let origin = OriginProducer(cacheCapacityBytes: 64 << 20)
        defer { withExtendedLifetime(origin) {} }
        let broadcast = try origin.createBroadcast(path: "bench").broadcast
        let tracks = try (0..<c.publishers).map {
            try broadcast.publishTrack(name: "\($0)", info: TrackInfo(timescale: 1_000_000))
        }
        let consumers = try tracks.flatMap { track in
            try (0..<c.subscribers).map { _ in try track.consume() }
        }
        let payload = Data((0..<c.size).map { UInt8($0 % 251) })
        func step(_ n: Int) async throws {
            try await withThrowingTaskGroup(of: Void.self) { readers in
                for consumer in consumers {
                    readers.addTask {
                        let received = try await consumer.recvGroup()
                        let group = try XCTUnwrap(received, "track ended")
                        for f in 0..<c.frames {
                            let received = try await group.readFrame()
                            let frame = try XCTUnwrap(received, "group ended")
                            XCTAssertEqual(frame.payload, payload)
                            XCTAssertEqual(frame.timestampUs, UInt64(n * c.frames + f))
                        }
                    }
                }
                for track in tracks {
                    let group = try track.appendGroup()
                    for f in 0..<c.frames { try group.writeFrame(payload, timestampUs: UInt64(n * c.frames + f)) }
                    try group.finish()
                }
                try await readers.waitForAll()
            }
        }
        for n in 0..<3 { try await step(n) }
        let start = ProcessInfo.processInfo.systemUptime
        let cpuStart = try cpu()
        var n = 3
        repeat {
            try await step(n)
            n += 1
        } while ProcessInfo.processInfo.systemUptime - start < budget
        let wall = ProcessInfo.processInfo.systemUptime - start
        let cpuUsed = try cpu() - cpuStart
        let mb = Double((n - 3) * c.delivered) / 1e6
        try broadcast.close()
        return [Result(name: "throughput/\(c.name)", unit: "MB/s", value: mb / wall),
                Result(name: "cpu/\(c.name)", unit: "ms/MB", value: cpuUsed * 1e3 / mb)]
    }
}
