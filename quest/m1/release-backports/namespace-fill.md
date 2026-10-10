# [S] NAMESPACE fill on d16+ on release

## Goal

On `release`, a draft 16+ SUBSCRIBE_NAMESPACE from a peer that does not send
SOLICIT still receives NAMESPACE entries for matching broadcasts, instead of
an empty stream.

## Plan

#5032 (`d5208239f`) fixed this on `main` on top of #4268's active counts.
For `release`, apply the solicitation filter only on drafts 14 and 15. Worth
doing if a Seattle peer discovers broadcasts through NAMESPACE rather than
unsolicited PUBLISH_NAMESPACE; otherwise this can be deleted.
