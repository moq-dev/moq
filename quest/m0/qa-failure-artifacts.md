# [S] A failing harness run leaves its run directory and a browser trace behind

## Goal

When smoke, WASM, or TS fails in CI, the run's logs, relay config, and a
Playwright trace of the failing page are downloadable from the workflow run.
Locally, the same directory survives the failure with the rerun command
printed. A passing run still cleans up.

## Plan

The harness library already gives every run a private directory with each
process's log, and `MOQ_TEST_KEEP=1` retains it; on failure it prints the
rerun command but still deletes the directory unless the flag was set. The
browser drivers record nothing. `smoke.yml` and `wasm.yml` upload nothing.

- Keep the run directory whenever the run fails, not only when asked.
- The Playwright drivers (`test/smoke/clients/js`, `test/wasm/driver.ts`)
  start a trace and save it into the run directory on failure. Chromium's
  console and page errors already reach the log.
- `smoke.yml` and `wasm.yml` upload the run directory as a workflow artifact
  when the job fails, with a short retention. The relay config in it carries
  only the harness's throwaway certificate and test tokens, so nothing needs
  redacting; the TS harness joins if its outputs fit the same shape.

That is the whole quest. Stack dumps of hung processes, relay qlog, HAR
capture, retained debug sessions, and fetching CI artifacts by run id were
the earlier plan, abandoned with #3506 as far more machinery than a failed
run needs; if one of them earns its place later it is its own quest.

## Related

- [Impaired path](/quest/m0/transport-impairment-profile.md) - records the profile and seed in the same directory
