# [S] Make token RSA tests independent of key-generation timing

## Goal

The token signing and verification tests retain algorithm coverage without failing because random RSA key generation exhausts the default test deadline.

## Plan

`js/token/src/key.test.ts` generates three RSA key pairs sequentially inside `RSA algorithms - sign and verify`. During concurrent local validation of PR #3453 with Bun 1.3.13, that test took 6656 ms and exceeded its 5000 ms deadline. The same unchanged test file passed all 49 tests in isolation with the same Bun version. This establishes a timing-sensitive test result, not a token implementation failure or a proven scheduling race.

- Measure key generation separately from signing and verification under concurrent suite load.
- Use committed non-production key fixtures for sign/verify assertions where key generation is not the behavior under test. Preserve algorithm and public/private key coverage.
- Keep any key-generation coverage separate, with a justified test boundary. Do not hide failures with retries or increase the whole suite timeout.
- Verify under the normal concurrent package test runner and show that malformed tokens and incorrect algorithm/key combinations still fail verification.
