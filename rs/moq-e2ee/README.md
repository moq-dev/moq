# moq-e2ee

`moq-e2ee` is now the `e2ee` module of
[`moq-crypto`](https://github.com/moq-dev/moq/tree/main/rs/moq-crypto), which
also holds content signing, and this final release contains no code.

The profile and wire are unchanged. Depend on `moq-crypto` and import from
`moq_crypto::e2ee` in place of `moq_e2ee`; see its
[MIGRATE.md](https://github.com/moq-dev/moq/blob/main/rs/moq-crypto/MIGRATE.md)
for the rest of the changes since `moq-e2ee` 0.0.20.
