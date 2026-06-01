import MoqFFI

/// Initialize logging with a level string: "error", "warn", "info", "debug",
/// "trace", or "" (defaults to info). Throws if called more than once.
public func logLevel(_ level: String) throws {
    try moqLogLevel(level: level)
}
