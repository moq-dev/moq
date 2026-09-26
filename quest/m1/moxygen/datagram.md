# [M] Datagram groups

## Goal

An IETF datagram that carries one object for a group is delivered as a
moq-lite datagram: one single-frame group, sequence preserved. Several
objects in one datagram group stay unsupported.

## Plan

moq-lite already does this. A datagram is a subscribe id, a group sequence, a
timestamp, and a payload. `insert_datagram` keeps the sequence so a relay
does not renumber it.

The IETF session does not read or write QUIC datagrams, so a datagram
subscribe delivers nothing. Copy the lite path onto that session. Do not
invent a second object list inside the group.

The moxygen cases with one object per group are the ones this can pass.

## Related

- [Moxygen compatibility](/quest/m1/moxygen/README.md) - the line this belongs to
