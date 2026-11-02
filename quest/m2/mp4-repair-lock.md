# [XS] moq repair refuses a recording still being written

## Goal

`moq export mp4 --output <FILE>` holds an advisory lock on its output while it records, and `moq repair <FILE>` refuses, with a message saying a recorder still has it open, when it can't take that lock. Neither side runs unlocked: where the filesystem can't lock, both refuse. Repairing a file mid-recording no longer corrupts it.

## Plan

Found by [moq-dev/moq#7560](https://github.com/moq-dev/moq/pull/7560), which adds `moq repair`: it can't tell a running `export mp4` still writes the file, and repairing one corrupts it. #7560 is still a draft, so this waits on it landing; `moq repair` and `moq_mux::container::mp4::repair` don't exist on `main` until then.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2. An advisory lock taken by the recorder and checked by repair, as #7560 suggested.

- The CLI owns the lock, not moq-mux: `repair` in moq-mux takes `impl Read + Seek` and stays file-agnostic. Use an exclusive `flock`-style lock on the open file (`std::fs::File::try_lock` is stable in Rust 1.89), held for the recording's life and released by the OS if the recorder dies, so a crashed recording stays repairable.
- Supported or refused (`AGENTS.md`), on both sides. The recorder fails at startup if it can't take the lock, a filesystem without lock support included, since its recording could never be repaired safely. `repair` takes the same lock non-blocking before scanning and refuses if it can't: held means a recorder still has the file open, and any other lock error (no lock support) refuses with that error rather than repairing unlocked.
- Test: a held lock makes `moq repair` refuse and leave the file untouched; #7560's nightly `export::a_killed_mp4_recording_is_repaired` still repairs after SIGKILL, since the lock dies with the process.
- Update `doc/bin/cli.md`'s `repair` entry.

Public API: none (CLI behavior only). Wire: none.

## Related

- [An interrupted mp4 recording keeps its last fragment](/quest/m2/mp4-sigint-flush.md) - the other half of recording durability
