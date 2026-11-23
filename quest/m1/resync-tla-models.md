# [M] Resync drifted TLA+ models before 0.22

## Goal

Every `verify-*` TLA+ model under `rs/moq-net/verify/` matches the current `main` implementation of its covered functions, passes its TLC configs, and records an up-to-date sync commit before 0.22 cuts.

## Plan

Found by the 2026-11-23 pre-0.22 test coverage audit (`890ea4e531`). Five of the six `verify-*` models drifted behind `HEAD` as late `m1`/`m2` lite-07 changes landed:

- `.agents/skills/verify-resume/SKILL.md`: sync `8c939bc9d0`, **56 commits** behind (`#9444`, `#9409`, `#9389`, `#9406`, `#9426`, `#9333`, `#9435`, `#9311`, `#9391`, `#9436`, `#9407`, and more)
- `.agents/skills/verify-failover/SKILL.md`: sync `46059266ec`, **53 commits** behind (`#9444`, `#9467`, `#9409`, `#9377`, `#9349`, `#9389`, `#9404`, `#9406`, `#9426`, `#9333`, `#9435`, and more)
- `.agents/skills/verify-subscribe/SKILL.md`: sync `e40a7a4340`, **22 commits** behind (`#9444`, `#9467`, `#9409`, `#9377`, `#9349`, `#9389`, `#9406`, `#9426`, `#9333`, `#9435`, `#9311`, and more)
- `.agents/skills/verify-credit/SKILL.md`: sync `e40a7a4340`, **9 commits** behind (`#9444`, `#9409`, `#9386`, `#9402`, `#9408`, `#9316`, `#9383`, `#9398`, `#9353`)
- `.agents/skills/verify-warm/SKILL.md`: sync `1399add3c8`, **3 commits** behind (`#9157`, `#8987`, `#8986`)
- `.agents/skills/verify-reconnect/SKILL.md`: 0 commits behind (current).

For each drifted skill, walk `git log <sync>..HEAD -- <covered-files>`, update the TLA+ spec and configs for any behavioral change (such as per-range `SUBSCRIBE_START`, `Max Age`, time-span anchors, and frame-level switch cutoff), run every config through `quest gate bash sh/tla.sh ...`, file or fix any real bug TLC uncovers, and advance the skill's sync commit to the PR's base. Also check `js/*/verify/` for drift.

Public API: none. Wire: none (unless TLC uncovers a lite-07 wire bug).
