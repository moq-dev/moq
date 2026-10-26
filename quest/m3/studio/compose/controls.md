# [M] Viewer controls

## Goal

A viewer composing a studio broadcast can switch off any element the layout
marks toggleable (a facecam, a chat overlay), and stops fetching its track.
The layout's per-element priorities steer delivery, so under congestion the
least important sources degrade first. The creator sets both per element in
the studio.

## Plan

Map element priority onto the subscription priorities moq-lite already has
(`doc/concept/moq-lite.md`), rather than a second mechanism. A toggled-off
element is unsubscribed, so it costs the viewer and the relay nothing, and
the pull model may stop its encode. Subscriptions are per track, not per
element: a linked source shown by several items stays subscribed while any
of them is on, at the highest priority among them.

## Required

- [Browser composition](/quest/m3/studio/compose/browser.md) - the viewer that exposes the toggles
