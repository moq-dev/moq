import Foundation
@_exported import MoqFFI

extension MoqSession {
    /// Graceful close. Documents the convention that code 0 means "no error".
    public func close() {
        cancel(code: 0)
    }
}

extension MoqBroadcastConsumer {
    /// Subscribe to a named audio rendition. Reads the catalog internally
    /// and waits for the rendition to appear before returning. Cancel by
    /// cancelling the enclosing Task.
    public func subscribeAudio(
        name: String,
        output: MoqAudioDecoderOutput
    ) async throws -> MoqAudioConsumer {
        let catalog = try subscribeCatalog()
        for try await update in catalog.updates {
            if let audio = update.audio[name] {
                return try subscribeAudio(name: name, catalogAudio: audio, output: output)
            }
        }
        throw MoqError.Closed(message: "catalog ended before \"\(name)\" appeared")
    }

    /// Subscribe to a named video rendition. Reads the catalog to find the
    /// container, then opens the media subscription. Cancel by cancelling
    /// the enclosing Task.
    public func subscribeVideo(
        name: String,
        maxLatencyMs: UInt64
    ) async throws -> MoqMediaConsumer {
        let catalog = try subscribeCatalog()
        for try await update in catalog.updates {
            if let video = update.video[name] {
                return try subscribeMedia(
                    name: name,
                    container: video.container,
                    maxLatencyMs: maxLatencyMs
                )
            }
        }
        throw MoqError.Closed(message: "catalog ended before \"\(name)\" appeared")
    }
}
