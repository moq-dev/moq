The wire layer, the JS counterpart of `rs/moq-net`. Keep their behavior identical.

# Rules

- Local and remote paths behave the same: a local close, cancel, or error reaches local readers exactly as the remote equivalent would, never as a cleaner end.
