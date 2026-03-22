---
title: "MoQ Largest Group Extension"
abbrev: "moq-largest-group"
category: info

docname: draft-lcurley-moq-largest-group-latest
submissiontype: IETF  # also: "independent", "editorial", "IAB", or "IRTF"
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

This document defines a Largest Group subscription filter type for MoQ Transport {{moqt}}.
A subscriber uses this filter to request delivery starting from the first object of the publisher's largest (most recent) group, ensuring a complete group is received.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Introduction
{{moqt}} Section 5.1.2 defines four subscription filter types that control where delivery begins:

- **Next Group Start (0x1)**: Starts at `{Largest.Group + 1, 0}`, skipping the remainder of the current group entirely.
- **Largest Object (0x2)**: Despite the name, starts at `{Largest.Group, Largest.Object + 1}` — the object *after* the largest, not the largest object itself.
- **AbsoluteStart (0x3)**: Starts at an explicit location specified by the subscriber.
- **AbsoluteRange (0x4)**: Starts and ends at explicit locations specified by the subscriber.

There is no filter that starts from the *beginning* of the current group.
Next Group Start skips the current group entirely, potentially adding latency equal to the group duration.
Largest Object starts mid-group, delivering objects that may depend on earlier objects in the same group.

Objects within a group are typically delta encoded (ex. video GOPs), so arbitrary objects in the middle of a group are undecodable without prior objects.
A subscriber using Next Group Start avoids this problem but must wait for the next group to begin, unnecessarily increasing join latency.

## Joining Fetch Workaround
{{moqt}} does provide a workaround: a subscriber can issue a separate "joining" FETCH request alongside a SUBSCRIBE to retrieve the beginning of the current group.
However, this approach has several drawbacks:

- **Complexity**: Libraries emulate Largest Group by coordinating a FETCH and SUBSCRIBE, splitting the group across multiple streams. This requires merging the results into a single coherent group and handling various edge cases, such as one of the two requests failing independently.
- **Head-of-line blocking**: If the group contains multiple sub-groups, the FETCH delivers them sequentially over a single stream, introducing head-of-line blocking that negates the benefits of sub-group parallelism.
- **Priority**: Everything should be delivered in dependency order to improve startup time and avoid potential flow control deadlocks. This requires prioritizing the FETCH higher than the SUBSCRIBE, which may be non-obvious or unsupported.

This extension avoids these issues by defining a Largest Group filter that starts delivery from the first object of the publisher's most recent group within the SUBSCRIBE itself, ensuring a complete group is delivered over the normal subscription path.
Additionally, the first group of a subscription behaves the same as any other group.


# Setup Negotiation
The Largest Group extension is negotiated during the SETUP exchange as defined in {{moqt}} Section 9.4.

Both endpoints indicate support by including the following Setup Option:

~~~
LARGEST_GROUP Setup Option {
  Option Key (vi64) = TBD1
  Option Value Length (vi64) = 0
}
~~~

If both endpoints include this option, the Largest Group filter is available for the session.
If a peer receives a SUBSCRIBE containing the Largest Group filter without having negotiated the extension, it MUST close the session with a PROTOCOL_VIOLATION.


# Largest Group Filter
This document defines a new subscription filter type for use in the Subscription Filter field of SUBSCRIBE messages as defined in {{moqt}} Section 5.1.2.

## Filter Type

~~~
Subscription Filter {
  Filter Type (vi64) = 0x20
}
~~~

The Largest Group filter uses Filter Type value `0x20`.
No additional fields (Start Location or End Group Delta) are present in the Subscription Filter.

## Semantics
When the publisher receives a SUBSCRIBE with the Largest Group filter, it computes the start location as:

~~~
{Largest.Group, 0}
~~~

Where `Largest.Group` is the group sequence number of the largest object known to the publisher at the time the SUBSCRIBE is processed.
Delivery begins from the first object (object 0) of that group.

The subscription is open-ended: the publisher continues delivering subsequent groups until the subscription is cancelled or the session ends.
This is consistent with the behavior of Next Group Start and Largest Object filters.

The largest group is delivered using normal SUBSCRIBE semantics.
If earlier objects in the group have been evicted from the cache, the publisher MAY attempt to repopulate the cache or simply drop the group as it would for any other subscription.


# Security Considerations
This extension introduces no new security considerations beyond those described in {{moqt}}.
The publisher already tracks group state; this filter uses existing state to compute the start location.


# IANA Considerations

This document requests the following registrations:

## MoQ Setup Option Types

This document registers the following entry in the "MoQ Setup Option Types" registry established by {{moqt}}:

| Value | Name | Reference |
|:------|:-----|:----------|
| TBD1 | LARGEST_GROUP | This Document |

## MoQ Subscription Filter Types

This document registers the following entry in the "MoQ Subscription Filter Types" registry established by {{moqt}}:

| Value | Name | Reference |
|:------|:-----|:----------|
| 0x20 | LARGEST_GROUP | This Document |


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Claude, an AI assistant by Anthropic.
