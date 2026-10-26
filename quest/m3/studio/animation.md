# [M] Animation and transitions

## Goal

Scene changes in `moq-studio` use transitions beyond a cut and fade: move
(items glide between their positions in the two scenes), slide, wipe, and
stinger (a video clip with a cut point). Any item property can be keyframed
with easing, so an item can fly in or a camera can push in, and the same
animation plays in every composition host.

## Plan

Move is OBS's most-installed plugin (Move Transition), and stingers are
standard in broadcast. Animate in moq-scene on the media clock and carry the
transition and keyframes in the layout track, so viewers compose them too.
Overlays animate through their own CSS. No Lottie runtime for now (decided
2026-10-26): the open ones either lack text (velato) or add a C++ build
(ThorVG). A stinger clip is a media source like any other.

## Required

- [Undo and the scene model](/quest/m3/studio/scene-model.md) - keyframes are scene model state
- [Layout track](/quest/m3/studio/compose/layout.md) - where transitions and keyframes travel
