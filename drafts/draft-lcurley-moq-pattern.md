---
title: "MoQ Pattern Extension"
abbrev: "moq-pattern"
category: info

docname: draft-lcurley-moq-pattern-latest
submissiontype: IETF
number:
date:
v: 3
area: wit
workgroup: moq

author:
 -
    fullname: Luke Curley
    email: kixelated@gmail.com

normative:
  moqt: I-D.ietf-moq-transport

informative:

--- abstract

This document defines pattern advertisements for MoQ Transport {{moqt}}.
A publisher can advertise namespaces it could serve without enumerating them, such as an archive or an on-demand processor.
Pattern matching, authorization, and negotiation do not require a cluster, hop identities, or route costs.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

A pattern is a sequence of segments over a namespace's tuple fields.
A path-based protocol can use the same matching semantics by treating each path segment as one field.

# Pattern Semantics {#patterns}

Each segment is a literal matching one field exactly, a wildcard matching any one field, a partial matching one field with a known prefix and suffix without overlap, or a globstar matching zero or more fields.
A pattern contains at most one globstar.
A namespace matches when its fields can be assigned to the segments in order, the globstar taking any number of them.
Without a globstar, the number of fields must equal the number of segments.
A pattern is exact; a subtree is a literal prefix followed by a globstar.

Patterns claim capability, not inventory: they say matching namespaces can be served, never that any exists.
An advertiser that will not serve a matching request refuses it as specified in {{resolution}}.
A receiver MUST NOT present a pattern as an available namespace.
It SHOULD combine duplicate pattern advertisements into one presented entry, withdrawn when the last advertiser retracts it.

## Rebasing {#rebasing}

A publisher answering SUBSCRIBE_NAMESPACE intersects each pattern with the requested literal prefix.
The result can have several residual patterns: each alignment of the globstar with the prefix yields a residual, sent as its own advertisement.
For example, a globstar followed by literal `a`, rebased under the prefix `a`, yields both the empty pattern (exactly that prefix) and the original globstar followed by `a` (deeper namespaces ending in `a`).
Identical residuals are sent once; a pattern with no match beneath the prefix is not sent.
The request prefix is matched before interpreting the residual, and its fields are literal.

## Authorization

A receiver MUST discard an advertisement not contained by what its sender may publish: every matching namespace must lie within the sender's authorized scope.
An over-wide claim is refused whole, not clamped to authorization.
A publisher MUST likewise restrict advertisements to what the receiver may learn; rebasing under a requested prefix does not grant authorization.
How authorization is expressed is out of scope.

# Setup Negotiation {#setup}

An endpoint declares support with this Setup Option:

~~~
NAMESPACE_PATTERNS Setup Option {
  Option Key (vi64) = 0x40B5C
  Option Value (vi64) = 1
}
~~~

The extension is enabled only when both endpoints send the value 1 in SETUP.
An absent option or any other value does not enable it.
An endpoint MUST process the peer's SETUP before sending pattern advertisements.
Unknown Setup Options are ignored as specified by {{moqt}}, so older peers do not enable the extension.
Negotiation is per session and independent of any clustering extension.
An endpoint MUST NOT send NAMESPACE_PATTERN to a peer that did not negotiate this extension, and MUST NOT forward a pattern to such a peer as a literal prefix.
A receiver MUST close the session with a PROTOCOL_VIOLATION if NAMESPACE_PATTERN arrives without negotiation.

# Namespace Advertisements {#namespace}

PUBLISH_NAMESPACE already carries Key-Value-Pair parameters; its NAMESPACE_PATTERN describes the full Track Namespace.
On a session that negotiated this extension, every NAMESPACE message appends the same parameter block used by PUBLISH_NAMESPACE:

~~~
NAMESPACE Message (Patterns) {
  Type (vi64) = 0x8,
  Length (16),
  Track Namespace Suffix (..),
  Number of Parameters (vi64),
  Parameters (..) ...
}
~~~

NAMESPACE_PATTERN describes only the suffix fields, relative to the literal SUBSCRIBE_NAMESPACE prefix ({{rebasing}}).
The parameter block is present even for ordinary prefixes, with a count of zero when empty.
If another negotiated extension also appends this block, there is one shared block containing all applicable parameters.
An endpoint MUST NOT append the block unless this extension or another extension defining that same block was negotiated.

## NAMESPACE_PATTERN Parameter {#namespace-pattern}
NAMESPACE_PATTERN makes the advertised namespace a pattern: one Segment Kind per tuple field, in order.

~~~
NAMESPACE_PATTERN Parameter {
  Type (vi64) = 0x40B59
  Length (vi64)
  Segment Kind (vi64) ...
}
~~~

|------|----------|-------------|
| Kind | Name     | Tuple Field |
|-----:|:---------|:------------|
| 0x0  | Literal  | The field's bytes. Matches exactly that field. |
|------|----------|-------------|
| 0x1  | Wildcard | Empty. Matches any one field. |
|------|----------|-------------|
| 0x2  | Globstar | Empty. Matches any run of zero or more fields; at most one per namespace. |
|------|----------|-------------|
| 0x3  | Partial  | Prefix Length (vi64), Prefix (..), Suffix (..). Matches any one field starting with Prefix and ending with Suffix without overlap. |
|------|----------|-------------|

Matching follows {{patterns}}. Without this parameter, the advertisement remains an ordinary namespace prefix.
A Partial's suffix runs to the end of its tuple field; the prefix length MUST fit within that field, and a match MUST be at least as long as the prefix and suffix together.
A receiver MUST close the session with a PROTOCOL_VIOLATION if the number of kinds differs from the number of tuple fields, a Wildcard or Globstar field is non-empty, a Partial's Prefix and Suffix are both empty, a Partial's prefix length exceeds its remaining field bytes, a second Globstar appears, or the pattern plus its requested prefix exceeds 32 fields.
Other kinds are reserved for extensions: a receiver MUST NOT select or forward an advertisement carrying one, but MUST otherwise process the message and retain its identity for withdrawal ({{withdrawal}}).


## Identity and Withdrawal {#withdrawal}

An advertisement's identity includes its tuple fields and the presence and complete Segment Kind list of NAMESPACE_PATTERN.
Two advertisements with identical tuple bytes but different kinds are distinct, as are a literal-only pattern and an ordinary prefix.
A publisher MUST NOT maintain duplicate advertisements of the same identity on one session; an update allowed by another negotiated extension retains that identity.

A PUBLISH_NAMESPACE advertisement is withdrawn by closing its request stream, as in {{moqt}}.
To withdraw a NAMESPACE advertisement, this extension appends parameters to every NAMESPACE_DONE on the negotiated session:

~~~
NAMESPACE_DONE Message (Patterns) {
  Type (vi64) = 0xE,
  Length (16),
  Track Namespace Suffix (..),
  Number of Parameters (vi64),
  Parameters (..) ...
}
~~~

For a pattern, the suffix and NAMESPACE_PATTERN MUST equal those of the advertisement being withdrawn, including unknown segment kinds.
For an ordinary prefix, NAMESPACE_PATTERN MUST be absent.
No routing metadata is needed for withdrawal.
A receiver MUST close the session with a PROTOCOL_VIOLATION if no active advertisement has that identity.
Closing the SUBSCRIBE_NAMESPACE response stream withdraws all its advertisements, including ignored ones.
An endpoint MUST NOT append this block unless the pattern extension was negotiated; negotiating clustering alone does not enable it.

# Request Resolution {#resolution}

A SUBSCRIBE, FETCH, or track-status request is resolved against the advertisements covering its requested namespace.
Only the most specific covering tier is consulted: more literal fields first, then no globstar over one, then more partials, then more wildcards, then more bytes pinned by partials, then a longer literal head.
An ordinary prefix is ranked as its literal fields followed by a globstar.
A refusal never falls through to a less specific tier.
Selection within the tier is local policy or the policy of another negotiated extension, such as clustering; patterns alone require neither a cost nor a Hop ID.

NO_CAPACITY ({{iana}}) refuses a request the publisher could serve but has no capacity for now.
It permits ONE re-resolution within the same tier, excluding the refusing advertiser, identified by its incoming session unless another extension supplies an origin identity.
A receiver that has spent its retry, or has no other candidate, MUST refuse downstream with a code other than NO_CAPACITY, so retries cannot compound hop by hop.
Every other refusal, including an unrecognized code, is terminal.
A receiver SHOULD NOT cache refusals.
A relay MUST NOT advertise a namespace merely because it resolved it; the advertiser SHOULD announce the concrete namespace once producing it.

# Security Considerations

A pattern may cover an arbitrarily large set of namespaces, but does not authorize access to any of them.
Receivers MUST enforce containment ({{authorization}}) for leading-wildcard patterns as well as literal-headed ones, and publishers MUST authorize each content request.
Implementations SHOULD bound concurrent advertisements and work started by matching requests, using NO_CAPACITY when capacity is exhausted.
Pattern support provides no loop detection; deployments that forward advertisements through a mesh need a separate routing mechanism.

# IANA Considerations {#iana}

This document requests the following registrations in the {{moqt}} registries.

## MOQT Setup Options

| Value   | Name               | Reference     |
|:--------|:-------------------|:--------------|
| 0x40B5C | NAMESPACE_PATTERNS | This Document |

The option is even, so its value is a bare varint. Only value 1 enables this extension.

## MOQT Message Parameters

| Value   | Name              | Carried In                                   | Reference     |
|:--------|:------------------|:---------------------------------------------|:--------------|
| 0x40B59 | NAMESPACE_PATTERN | PUBLISH_NAMESPACE, NAMESPACE, NAMESPACE_DONE | This Document |

The parameter is odd, so its value is a length-prefixed byte string of segment-kind varints.

## MOQT Error Codes

| Value   | Name        | Registry            | Reference     |
|:--------|:------------|:--------------------|:--------------|
| 0x40B5A | NO_CAPACITY | REQUEST_ERROR Codes | This Document |

--- back
