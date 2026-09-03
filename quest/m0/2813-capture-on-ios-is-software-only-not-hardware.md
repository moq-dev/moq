# [XS] Capture on iOS is software only , not hardware

## Goal

Implement and verify the behavior tracked in [#2813](https://github.com/moq-dev/moq/issues/2813)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming into two independent halves: (a) verify
whether iOS Safari can be coaxed into hardware encode and document the result
(dev already prefers Safari's hardware codecs), and (b) fix the permission
re-prompt on refresh, which is the actionable web bug.

### Issue context

<img width="1290" height="2796" alt="Image" src="https://github.com/user-attachments/assets/0b481728-fa5d-44d2-8b45-dc6b13048800" />

https://moq.dev/publish/

It also always asks for camera and audio access on browser refresh .

## Closes

- [#2813](https://github.com/moq-dev/moq/issues/2813) - close this issue when the quest finishes
