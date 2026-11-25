# moq-cade

## Goal

moq-cade replaces MoQ Boy: a multiplayer retro console demo. Viewers on the
demo page pick one of a game's controllers and play live with or against each
other over MoQ, and each one sees how many viewers hold each pad. It runs NES,
SNES, and Genesis games through libretro cores.

Non-goals: rollback netcode, accounts, persistent lobbies, hardware-rendered
cores (N64, PS1 with GL), and cores without a free license.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan):

- Outcome: multiplayer on consoles with more than one pad, replacing the Game
  Boy outright (✅ "Yes, but drop the GB" (rejected: "Yes, as stated", which
  kept the GB alongside; "Drop")). One console pad is the whole Game Boy story,
  and real multiplayer on it would need two linked emulators.
- Name: moq-cade, with `<moq-cade>` and `demo/cade` (✅ moq-cade (rejected:
  moq-nes and keeping MoQ Boy, which both carry a Nintendo trademark in a
  public name and neither fits more than one console)).
- Pads: team anarchy. Each viewer picks a pad, and that pad is pressed while
  any of its pickers holds the button. This is today's union rule, applied per
  pad (✅ team anarchy (rejected: exclusive seats, where an idle tab squats a
  pad forever without an idle timer; seats with crowd fallback, which is the
  most state and UI for little gain)).
- Emulation: a libretro frontend that loads cores at runtime (✅ libretro
  (rejected: native Rust cores, where the only Rust SNES/Genesis option is
  GPL-3 jgenesis, so static linking would make the binary GPL; NES only through
  tetanes-core)). Each extra console is a core, a pad layout, and a ROM.
- Core licenses: free licenses only: GPL, MIT, or MPL, as nixpkgs packages them
  (✅ free only (rejected: allowing snes9x, genesis-plus-gx, picodrive, and
  fbneo, which are non-commercial, an unclear fit for a demo that advertises a
  product)).
- Consoles: NES, SNES, and Genesis (✅ those three (rejected for now: GB/GBC
  through sameboy, which is single-player; GBA through mgba, which is
  crowd-only; N64, which needs a headless GL context)).
- Placement: m4. This is a feature for an unpublished demo, and nobody is
  waiting on it.

ROMs must be freely redistributable homebrew with a multiplayer mode,
hosted on `rom.moq.dev` like today's. Record each ROM's license and source
in `demo/cade/justfile` beside its recipe. Uploading one takes Cloudflare R2
credentials (`just upload`). If the implementer has none, the PR adds a
`Maintainer:` item to its quest's `Waiting` with the exact `just upload`
commands, and the demo runs locally from downloaded files meanwhile.
