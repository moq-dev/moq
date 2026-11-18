# [S] An abort on a finished JS window reaches undrained readers

## Goal

Window.Finished.abort(error) releases the finished window and reports the error to readers still draining it, matching Rust.

## Plan

The handle from [moq-dev/moq#8395](https://github.com/moq-dev/moq/pull/8395) returns its producer after finish sets the terminal guard. Publishing two records, consuming one, then calling finish().abort(error) delivered the second and clean EOF. Exercise an undrained reader and normal finish through the public API.
