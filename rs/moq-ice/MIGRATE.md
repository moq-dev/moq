# Migrating moq-ice

How to update your code for each breaking change, newest first.

## Unreleased

### Construct `Socket` with advertised candidates

Replace struct literals with `Socket::new(udp, locals, inbound)?`.
It refuses empty `locals` immediately with `std::io::ErrorKind::InvalidInput`; its fields are private so candidates cannot be emptied later.
Read candidates through `socket.locals()`.
This crate remains published for `moq-tokio` and `moq-rtc`, as internal infrastructure with no API stability promise.
