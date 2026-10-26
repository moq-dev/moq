# Address groups by timestamp

## Goal

Groups are addressed by their start timestamp instead of a sequence number,
in the moq-net API (Rust and `@moq/net`) and the lite-07 wire. A reader seeks
or starts from a time, nothing maps between group numbers and time, and no
producer has to remember group numbers. Old subscribers, old publishers, and
published JS clients keep working. VOD, FETCH, and archives need no backwards
compatibility.

## Required

- [Rust groups by start](/quest/m1/time-addressing/model.md) - the moq-net model and API address groups by start, and legacy wires carry the start ticks as the group ID
- [Lite-07 by start](/quest/m1/time-addressing/lite07.md) - lite-07 carries starts, previous-start links, GROUP_END, and time bounds instead of sequence numbers
- [moq-transport starts and ends](/quest/m1/time-addressing/ietf.md) - Prior Group ID Gap, TIMESTAMP, and END_OF_GROUP carry links, PTS, and ends
- [@moq/net by start](/quest/m1/time-addressing/js.md) - JS matches Rust's model, wire, and legacy mapping
- [Consumers by start](/quest/m1/time-addressing/consumers.md) - hang drops duration and discontinuity markers, and every consumer that numbered groups uses starts
- [Bindings by start](/quest/m1/time-addressing/bindings.md) - moq-ffi, moq-c, and every binding expose starts, with migration docs
- [E2EE nonce](/quest/m1/time-addressing/e2ee.md) - the nonce binds the start instead of the sequence
- [Guide](/quest/m1/time-addressing/guide.md) - a page explaining seek by time, group ends, restarts, strictly increasing starts, and how legacy peers see group IDs
