# [S] TLS settings that would be ignored are refused at load

## Goal

A relay or `moq` CLI config that sets a TLS option the configured listeners
would never use fails at load with an error naming the option, instead of
starting and silently ignoring it. Known cases: `tls.peers` on a server with
no QUIC listener, and `web.https.root` without `web.https.listen`.

## Plan

Same class of bug as the listener client CA that #4912 now refuses in
`moq_tokio::Server::build`. Audit the other TLS and listener options for
settings that only take effect on a listener that may not exist, and refuse
each where the config is assembled. TOML cannot express clap's `requires`, so
the check belongs in validation, not in the flag definitions.

Public API: none. Behavior: configs that relied on an ignored option now fail
to start, so note it in `doc/setup/upgrade.md`.
