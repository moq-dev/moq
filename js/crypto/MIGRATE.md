# Migrating @moq/crypto

## Unreleased

### Protected datagrams use their own tracks ([#7846](https://github.com/moq-dev/moq/pull/7846))

Call `E2EE.Broadcast.Producer.createDatagrams(semantic, info)` and
`E2EE.Broadcast.Consumer.datagrams(semantic)`. Their `E2EE.Datagram.Producer` and
`Consumer` use `append`, `insert`, and `recv` in place of the stream track's
`appendDatagram`, `insertDatagram`, and `recvDatagram`. A datagram's sequence is
independent of same-name groups; the existing separate cryptographic key domains
keep their nonces distinct. Stream authentication failures and lifecycle operations
no longer end the datagram track. Finish or abort its producer, and close its consumer,
independently.
