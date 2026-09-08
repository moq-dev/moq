# Cross-language harnesses

Tests that span more than one language or need a server. Per-language unit tests
live in each language's own justfile.

| Harness                | Runs                                              |
| ---------------------- | ------------------------------------------------- |
| [smoke](smoke/)        | the publish x subscribe interop matrix            |
| [wasm](wasm/)          | the `@moq/wasm` bindings in headless Chromium     |
| [ts](ts/)              | MPEG-TS/IRD compliance on the exporter's output   |
| [lib](lib/bundle.sh)   | the debug bundle all three write into             |

## Debug bundles

Every run of a harness gets one directory, and the harness writes its scratch
there instead of into a `mktemp -d`. So the evidence a failure needs accumulates
as a side effect of running, rather than as a capture step that can be skipped
or arrive too late. A passing run deletes the directory; a failing one keeps it,
bounds it, redacts it, and describes it.

```text
target/qa/smoke-20260907T230859Z-71169/
  manifest.json     run identity, endpoints, results, capabilities, rerun command
  versions.txt      the toolchain that produced the run
  teardown.sh       reaps exactly the processes this run owned
  session.md        only with MOQ_QA_RETAIN: URLs, PIDs, attach + teardown
  work/             per-process logs, relay config, captures, per-cell timings
  trace/            Playwright traces, screenshots, HARs, page-error logs
  stacks/           backtraces of whatever was still running
  qlog/             relay QUIC traces, only with MOQ_QA_QLOG
```

`manifest.json` is the file to open first. It names the commit the run was built
from and whether the tree was dirty, the endpoints and the protocol each one
serves, every binary by SHA-256 so a backtrace can be matched to its symbols,
every fixture by SHA-256 so a rerun that behaves differently can be told from
one fed different bytes, each result with the time it took, and the exact
command that reproduces the run.

Each run writes a new directory, so re-running to investigate never overwrites
the bundle that captured the original failure.

### Knobs

| Variable           | Effect                                                    |
| ------------------ | --------------------------------------------------------- |
| `MOQ_QA_ARTIFACTS` | where bundles go (default `target/qa`)                     |
| `MOQ_QA_KEEP=1`    | keep the bundle even when the run passes                   |
| `MOQ_QA_RETAIN=1`  | on failure, leave the run's processes alive                |
| `MOQ_QA_QLOG=1`    | build the relay with `--features qlog` and capture traces  |
| `MOQ_QA_LOG_CAP`   | per-text-file byte budget (default 2 MiB, `0` disables)    |
| `MOQ_QA_FILE_CAP`  | per-binary-file byte budget (default 8 MiB, `0` disables)  |
| `MOQ_QA_STACKS=0`  | skip stack capture                                         |

### What a bundle can and cannot see

Every capability is recorded by name in the manifest, including the ones that
were unavailable. A capability nothing recorded looks exactly like one that was
never tried, which is what sends a reader hunting for a trace that was never
going to exist.

- **Browser.** A failing browser case leaves a Playwright trace with DOM
  snapshots and screenshots, a screenshot of the final state, the page's console
  and error transcript, and a HAR. Open the trace with
  `bunx playwright show-trace <file>`.
- **Network.** The HAR covers HTTP and nothing else, and its bodies are omitted.
  The media itself rides WebTransport over QUIC, which neither the trace nor the
  HAR can see at all. Relay qlog is the only view of that, and it needs a relay
  built with `--features qlog`, so it is opt-in: `MOQ_QA_QLOG=1`. When it is
  requested and the backend writes nothing, the manifest says so rather than
  leaving an empty directory.
- **Stacks.** A subscriber still running as its budget expires is stack-dumped
  before its own timeout kills it, and so is anything still alive when a failing
  run tears down. Capture is best-effort and bounded: a debugger that cannot
  attach (Linux `ptrace_scope`, macOS hardened runtime, no debugger installed)
  records the refusal instead of blocking cleanup. It walks the process tree
  from the harness shell down, so it can catch the shell alone if the client has
  already gone; the file says when that happened rather than passing a shell's
  backtrace off as the client's.
- **Credentials.** The harnesses run anonymous against a self-signed localhost
  relay, so there is nothing to leak by design. Under that, everything retained
  is swept for JWT-shaped tokens, token query parameters, authorization and
  cookie headers, and URL credentials before it can be uploaded.
- **Not collected.** Packet payloads and core dumps. Both carry far more than
  the failure needs, and neither is safe to upload, so they stay a local opt-in
  you arrange yourself (`tcpdump`, `ulimit -c`) rather than something a harness
  turns on.

### Retained sessions

`MOQ_QA_RETAIN=1` leaves a failing run's processes alive, holding their ports,
so the relay can be attached to rather than reconstructed. The bundle then also
carries `session.md`, listing the endpoint URLs, the live PIDs, the debugger
command for each, the external path where live logs continue, and the teardown
command. The bundle's `work/` remains a bounded, redacted failure-time snapshot.
Nothing else reaps them:

```bash
MOQ_QA_RETAIN=1 just test smoke
bash target/qa/smoke-*/teardown.sh
```

`teardown.sh` kills only the processes that run recorded, and only while their
process start time still matches, so a PID the kernel has since handed to
somebody else is left alone even if it runs the same command.

### From CI

A failing job uploads its bundles as a `qa-bundle-*` artifact with finite
retention. To open the same failure a reviewer is looking at:

```bash
just test fetch 12345678901   # or paste the run URL
```

### Drills

The bundle only does its job on the failure path, so the failure path is what
has to be exercised. `just test bundle` self-tests the library, and `smoke.sh`
can inject the three failures that matter, each of which must exit non-zero,
leave an inspectable bundle, and reap its children:

```bash
# a browser assertion fails mid-playback: trace, screenshot, console transcript
MOQ_QA_FAULT=browser just test smoke --publishers js --subscribers js --timeout 30

# the relay is killed under a live matrix: relay log, its stack, client logs
MOQ_QA_FAULT=relay just test smoke

# no publisher starts, so every subscriber hangs: stacks taken before the kill
MOQ_QA_FAULT=hang just test smoke --subscribers rust,c
```
