# Browser WebTransport fixes we write ourselves

## Goal

The browser WebTransport bugs that moq's quests wait on are fixed by patches
we write, so each wait ends on our schedule instead of a vendor's, and the
workarounds they forced become deletable once the fix reaches a stable
release. Each child writes one fix, proves it against moq's reproduction in a
local build of that browser, and hands it to the maintainer to submit.

Not included: new web APIs (the receive-credit contract
[Browsers expose WebTransport receive credit](/quest/m4/browser-receive-credit.md)
waits on is W3C spec work), and carrying browser forks: a patch upstream
rejects is reported back to the maintainer, not maintained.

## Plan

Decided 2026-11-25 (maintainer, /quest-plan from a brainstorm of outlandish
improvements): ✅ write the fixes ourselves for the four bugs below
(rejected: also propose a W3C receive-credit API; keep waiting on vendors).

Decided 2026-11-25 (maintainer, /quest-plan): ✅ agents build, the maintainer
submits (rejected: contract a browser consultancy such as Igalia; agents
submit under the maintainer's account while the maintainer answers
reviewers). Chromium's
[AI coding policy](https://chromium.googlesource.com/chromium/src/+/main/agents/ai_policy.md)
and Firefox's [AI coding policy](https://firefox-source-docs.mozilla.org/contributing/ai-coding.html)
both accept AI-assisted patches but need a human author who understands the
patch and answers human reviewers personally; WebKit's repository ships its
own `.claude/` config. Posting outside moq's organizations also needs the
maintainer's go-ahead each time.

So each child delivers, without posting anything upstream:

- the root cause, found by reading and instrumenting a local build, then
  reproduced with moq's reproduction before the fix;
- the fix, with the regression test the browser's project expects (a
  web-platform-tests case where the behavior is spec-visible, otherwise the
  component's unit test);
- the reproduction passing on the patched build and failing on the unpatched
  one;
- a patch description in the project's format, flagging any part the
  implementer is unsure of, as Chromium's policy asks.

Recommended: the patch lives on a branch of a fork under `kixelated` on
GitHub, which the maintainer submits from (a WebKit pull request,
`git cl upload` for Chromium, `moz-phab` for Firefox, a pull request for
neqo). A child's finishing PR deletes it and adds a `[XS]` waiting quest in
this epic whose `Waiting` holds `Maintainer: submit <branch> to <project>
and answer its review`, then the project's merge and stable release; that
quest's end clears the matching wait elsewhere in the tree.

Browser builds are large: Chromium wants a Linux host with ~100 GB free and a
long first build, and WebKit's Cocoa port needs macOS with Xcode. Build each
once per host and reuse the tree; never put it in `target/`.

## Related

- [Report approved upstream bugs](/quest/m4/upstream-reports.md) - non-WebTransport browser bugs we only report
- [Safari WebTransport returns](/quest/m5/safari-webtransport.md) - what the WebKit fix unblocks
- [Send a WebTransport close's capsule without waiting](/quest/m5/chromium-close-quiesce.md) - the workaround the Chromium close fix lets us delete
