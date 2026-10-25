# [S] mbx keeps a live build's trees on a full disk

## Goal

A CI job never rebuilds moq-ffi mid-run because mbx garbage collection
evicted the generated source trees its build is using, and when a Dart build
hook does rebuild, its error names the cause.

## Plan

Seen three times in about 34 hours (merge-group runs of
[moq-dev/moq#6449](https://github.com/moq-dev/moq/pull/6449),
[moq-dev/moq#6256](https://github.com/moq-dev/moq/pull/6256), and
[moq-dev/moq#6436](https://github.com/moq-dev/moq/pull/6436)): the runner
disk dropped below mbx's 50 GiB free-space minimum, gc evicted trees the
job had restored, and the Dart build hook rebuilt moq-ffi. Then
`sh/dart/check.sh` reported "update hook_vars in sh/dart/generate.sh", which
sends the reader to the wrong file.

- Either mbx gc skips trees a live build holds, or the runner keeps enough
  free disk that gc never runs mid-job; pick by measurement.
- The check tells the two causes apart before naming one: it blames
  `hook_vars` only when they differ from `sh/dart/generate.sh`, and otherwise
  reports that the restored tree was missing.

Public API: none. Wire: none.
