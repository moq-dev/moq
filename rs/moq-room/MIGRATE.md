# Migrating moq-room

How to update your code for each breaking change, newest first.

## Unreleased

### Async wrappers are opt-in

Enable the `async` Cargo feature to keep awaiting `Room::next`, `chat::Publisher::expire`,
`chat::Subscriber::recv`, and `chat::Subscriber::subscribe`. The feature is off by default;
`poll_next`, `poll_expire`, `poll_recv`, and `chat::Subscriber::new` stay available without it.

### `moq_net::Path` is bounded

`broadcast_path` returns `Result<Path, moq_net::path::Error>`, failing when the identity and kind exceed `moq_net::Path::MAX_BYTES`. `claims` takes `impl TryInto<moq_net::Path, Error: Into<moq_net::Error>>` and fails with `Error::Net` on a room or identity that long.

### `chat::PRIORITY` is signed (#6142)

`chat::PRIORITY` is an `i8`, still 10.

### Chat events carry their send time

`chat::Subscriber::recv` / `poll_recv` return `moq_net::Timed<chat::Event>`: `.value` is the event they used to return, and `.at` is its frame's timestamp.
