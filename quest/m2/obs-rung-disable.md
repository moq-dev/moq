# [S] OBS disables simulcast rungs the grant cannot fund

## Goal

With several OBS video encoders publishing renditions of one source
(`video_tracks` in `cpp/obs/src/moq-output.cpp`), the obs-moq output stops
the top renditions' encoders when their bandwidth grant cannot fund them,
advertising each as `enabled: false`, and resumes them once the grant
recovers. The lower renditions keep their share.

## Plan

Apply `moq_mux::rate::Rung` per encoder, through the generated C++ package,
in the same place [Rate control](/quest/m2/obs-moq-video/rate-control.md)
puts the plain `Control`. Give each encoder a priority below the next lower
one's so the allocator fills lower renditions first, and keep each
reservation across demand gaps so a rung its viewers left can still resume.
Verify with `just obs compile`, `just obs test`, and a shaped uplink; document
the behaviour in `doc/bin/obs.md`.

## Required

- [Rate control](/quest/m2/obs-moq-video/rate-control.md) - the plugin follows its grant at all, and settles where the policy lives

## Related

- [moq-ffi rung disable](/quest/m2/simulcast-rung-disable-ffi.md) - the FFI video producer applying the same rule
- [OBS multitrack](/quest/m2/obs-multitrack.md) - OBS's native multitrack encoders as the renditions this disables
