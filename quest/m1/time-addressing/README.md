# Address groups by timestamp

## Goal

Groups are addressed by their start timestamp instead of a sequence number,
in the moq-net API (Rust and `@moq/net`) and the lite-07 wire. A reader seeks
or starts from a time, nothing maps between group numbers and time, and no
producer has to remember group numbers. Old subscribers, old publishers, and
published JS clients keep working. VOD, FETCH, and archives need no backwards
compatibility.

## Required

- [Spike](/quest/m1/time-addressing/spike.md) - a Rust and JS prototype measures what breaks, and the spike writes the design and the implementation quests for the cut
- [Guide](/quest/m1/time-addressing/guide.md) - a page explaining seek by time, group ends, restarts, strictly increasing starts, and how legacy peers see group IDs
