// Subscribe-only cross-language interop client for the smoke test, built on the
// ergonomic Swift API of the published moq-dev/moq-swift package (the `Moq`
// module's Client/Session/BroadcastConsumer classes and AsyncSequence streams),
// not the raw MoqFFI handles. Connect, find the video track in the catalog, and
// exit 0 as soon as any non-empty frame arrives (1 on timeout).
//
//   smoke subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20
//
// Publishing isn't wired up: the raw-stream importer the other clients publish
// with isn't exercised here, so this client only subscribes.
import Foundation
import Moq

enum SmokeError: Error { case timeout, noVideo, noData }

struct Args {
    var url = ""
    var broadcast = ""
    var timeout = 20.0
}

func parseArgs() -> Args {
    var a = Args()
    let argv = CommandLine.arguments
    var i = 1
    while i < argv.count {
        switch argv[i] {
        case "--url": i += 1; if i < argv.count { a.url = argv[i] }
        case "--broadcast": i += 1; if i < argv.count { a.broadcast = argv[i] }
        case "--timeout": i += 1; if i < argv.count { a.timeout = Double(argv[i]) ?? 20.0 }
        default: break // leading "subscribe" positional and anything else ignored
        }
        i += 1
    }
    return a
}

func warn(_ s: String) { FileHandle.standardError.write((s + "\n").data(using: .utf8)!) }

// A catalog update that actually carries a video track. A lazy publisher may
// announce video in a later update, not the first snapshot. CatalogConsumer is
// an AsyncSequence of catalog snapshots.
func videoTrack(_ bc: BroadcastConsumer) async throws -> (String, Video) {
    for try await update in try await bc.subscribeCatalog() {
        if let first = update.video.first { return (first.key, first.value) }
    }
    throw SmokeError.noVideo
}

func subscribe(_ args: Args) async throws {
    // The smoke relay uses a self-signed cert. connect(to:) wires an auto-created
    // origin, so session.consumer discovers announcements without touching MoqFFI.
    let client = Client()
    client.setTlsVerify(false)

    let session = try await client.connect(to: args.url)
    defer { session.shutdown() } // graceful close

    let announced = try session.consumer.announcedBroadcast(path: args.broadcast)
    let bc = try await announced.available()

    let (name, video) = try await videoTrack(bc)
    let media = try await bc.subscribeMedia(name: name, container: video.container)

    var total = 0
    for try await frame in media {
        total += frame.payload.count
        if total > 0 {
            warn("received \(total) bytes from \(args.broadcast)")
            return
        }
    }
    throw SmokeError.noData
}

func withTimeout(_ seconds: Double, _ op: @escaping () async throws -> Void) async throws {
    try await withThrowingTaskGroup(of: Void.self) { group in
        group.addTask { try await op() }
        group.addTask {
            try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            throw SmokeError.timeout
        }
        try await group.next()
        group.cancelAll()
    }
}

let args = parseArgs()
if args.url.isEmpty || args.broadcast.isEmpty {
    warn("usage: smoke subscribe --url U --broadcast B [--timeout S]")
    exit(2)
}

do {
    try await withTimeout(args.timeout) { try await subscribe(args) }
    exit(0)
} catch SmokeError.timeout {
    warn("error: timed out waiting for data")
    exit(1)
} catch {
    warn("error: \(error)")
    exit(1)
}
