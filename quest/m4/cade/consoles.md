# [M] moq-cade runs SNES and Genesis games

## Goal

moq-cade runs a multiplayer SNES game and a multiplayer Genesis game on the
demo page, next to the NES game, each with its console's pad layout.

## Plan

The decisions are in the [epic](/quest/m4/cade/README.md). Recommendations:

- SNES: nixpkgs `libretro.mesen-s` or `libretro.bsnes` (both GPL-3). bsnes is
  the more accurate and the more costly, so measure CPU per instance and pick.
  The pad adds X, Y, L, and R. SNES switches to 512-wide hi-res modes mid-game,
  which exercises the geometry handling from
  [libretro](/quest/m4/cade/libretro.md).
- Genesis: nixpkgs `libretro.blastem` (GPL-3). The free alternatives are
  non-commercial (genesis-plus-gx, picodrive), and the epic rejected them. If
  blastem's libretro core can't run the chosen ROMs, stop and bring that to
  the maintainer rather than loosening the license rule. Offer the 6-button pad
  (A, B, C, X, Y, Z, Start, Mode); a 3-button game ignores the extra buttons.
- The web pad layout follows the console the game's catalog or status names.
  It is not inferred from the ROM file name.

Public API: none. Wire: none.

## Required

- [libretro](/quest/m4/cade/libretro.md) - the frontend, pad selection, and the rename this builds on
