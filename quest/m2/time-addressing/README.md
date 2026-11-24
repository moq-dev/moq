# Address groups by timestamp

## Goal

Groups are addressed by their start timestamp instead of a sequence number,
in the moq-net API (Rust and `@moq/net`) and the lite-07 wire. A reader seeks
or starts from a time, nothing maps between group numbers and time, and no
producer has to remember group numbers. Old subscribers, old publishers, and
published JS clients keep working. VOD, FETCH, and archives need no backwards
compatibility.
