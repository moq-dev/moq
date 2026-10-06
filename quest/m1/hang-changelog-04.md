# [XS] Hang draft changelog matches what -03 published

## Goal

The `drafts/draft-lcurley-moq-hang.md` changelog lists under -03 only what the
published -03 contains. Entries that landed after -03 was published (delay,
group starts, namespaced keys) move to the -04 section #4915 opens.

## Plan

Compare against the published -03 on the datatracker. Land after #4915. Run
`just drafts check`.
