# [M] Config merge: empty lists lose to the environment, and the file outranks it

## Goal

Implement and verify the behavior tracked in [#3051](https://github.com/moq-dev/moq/issues/3051)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

Two related defects in how `moq-relay` and `moq-bench` resolve configuration. Both predate the Usage migration (#3030); one of them that migration briefly widened, and #3030 fixes that part.

#### 1. A bare `Vec<T>` loses to the environment

The TOML merge parses CLI + env + defaults, overlays the file, then re-parses the CLI so explicit flags win. That last step fills any field the parser reads as *empty* -- from the environment and from declared defaults, not just from the command line.

Emptiness is a property of the type. A `Vec<T>` reads empty when it has no items, so a config file that deliberately sets an empty list is refilled from the environment:

```toml
[connect]
version = []     # "offer every supported version"
```

With `MOQ_CONNECT_VERSION=moq-lite-01` set, this resolves to `moq-lite-01` instead.

Affects roughly fifteen env-bound list fields: TLS certificate/key/root lists, accepted versions on both the connect and listen sides, the Unix peer allowlists (`uid`/`gid`/`pid`), cluster peers, auth domains, and the web HTTPS material.

Booleans had the same bug and are now `Option<bool>` with the default resolved in code. The same fix works for lists (`Option<Vec<T>>`) but multiplies a convention rather than removing the cause -- see below.

#### 2. The file outranks the environment

Precedence today is `CLI > TOML > env > defaults`. That ordering was never chosen: it falls out of the implementation, which folds env in during the parse and then overlays the file on top of everything. The sentence documenting it in `doc/bin/relay/config.md` was written to describe the behavior after the fact.

Nearly every comparable tool does `CLI > env > file` (12-factor, Viper, and `usage-config`'s own default). The reasoning is deployment: the file ships inside the artifact and the environment varies per deployment, so file-wins means rebuilding an image to change one setting, and means a secret injected by an orchestrator cannot override a placeholder in a checked-in file.

#### Why they are one issue

Both come from the same missing thing: nothing records *which source set a value*. Presence is inferred from the value itself, so "set to false", "set to the empty list", and "never set" are indistinguishable -- and any fix built on comparing values inherits the same blind spot one level down ("set to the default value" vs "never set").

`usage-config` exists for this. `CliLayer` is built from what the parser saw rather than from the parsed struct, `EnvLayer` reads variables against a registry that declares which key each one backs, and `Layers` takes the order from the caller, so the precedence flip in (2) becomes a declared line rather than an emergent property. It also yields provenance, so the relay could answer "where did this setting come from".

The cost is real and worth stating: `usage::Cli` and `usage::Config` share the `#[usage(...)]` attribute namespace and each rejects the other's attributes, so one struct cannot carry both derives. A registry means declaring every merged setting a second time -- roughly 100 for the relay, 20 for the bench -- with `Registry::drift` as a test that the two declarations stay in step.

#### Suggested order

1. `Option<Vec<T>>` for the fifteen list fields, as a contained fix for (1) with a regression test.
2. Then evaluate the registry for (2), where the duplication buys the precedence flip and provenance rather than just a bug fix.

Doing (2) first would make (1) unnecessary, so it is worth deciding before spending on (1).

## Closes

- [#3051](https://github.com/moq-dev/moq/issues/3051) - close this issue when the quest finishes
