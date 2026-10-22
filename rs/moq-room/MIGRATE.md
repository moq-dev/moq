# Migrating moq-room

How to update your code for each breaking change, newest first.

## Unreleased

### `chat::PRIORITY` is signed (#6142)

`chat::PRIORITY` is an `i8`, still 10.

### Chat events carry their send time

`chat::Subscriber::recv` / `poll_recv` return `moq_net::Timed<chat::Event>`: `.value` is the event they used to return, and `.at` is its frame's timestamp.
