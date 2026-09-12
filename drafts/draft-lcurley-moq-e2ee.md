---
title: "MoQ End-to-End Encryption Profile"
abbrev: "moq-e2ee"
category: info

docname: draft-lcurley-moq-e2ee-latest
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
  moql: I-D.lcurley-moq-lite
  moqt: I-D.ietf-moq-transport
  RFC4648:
  RFC5116:
  RFC5869:

informative:
  sframe: RFC9605
  secure: I-D.ietf-moq-secure-objects
  aeadlimits: I-D.irtf-cfrg-aead-limits
  hang: I-D.lcurley-moq-hang

--- abstract

This document specifies moq-e2ee-01, a versioned profile for end-to-end encryption of MoQ application payloads.
Authorized publishers and subscribers share a 32-byte broadcast secret out of band.
HKDF-SHA-256 derives opaque physical track names and per-track AES-128-GCM keys; grouped frames and datagrams use separate key domains.
Media frames and datagrams carry only ciphertext plus a 16-byte tag.
The profile binds object identity through derivation and the nonce, not an on-wire header.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Introduction
MoQ relays forward named tracks of groups and frames ({{moql}}, {{moqt}}) without parsing application payloads.
This profile encrypts those payloads and the semantic track names that would otherwise describe them, so a relay cannot recover content.
It reuses AES-128-GCM and the 96-bit group/frame nonce shape of {{secure}} where those identities map, and specifies the moq-lite and datagram bindings that draft does not cover.

The profile does not distribute keys, sign senders, pad payloads, or rotate a key inside a generation.
Applications that need those properties terminate this profile and run a different one.


# Relationship to Other Formats {#related}
{{secure}} encrypts a MoQ Transport object under a per-track base key.
Its nonce is a per-track salt XOR `uint64_be(group) || uint32_be(object)`, its AAD includes publisher priority and immutable properties, and the Key ID rides in those properties.
The object payload is a length-prefixed plaintext plus an optional encrypted-properties list.

This profile keeps AES-128-GCM, HKDF-SHA-256, a 96-bit nonce of `uint64_be(group) || uint32_be(frame)`, and the rule that an identity is used at most once.
It differs where the models diverge:

- One 32-byte broadcast secret authorizes every track. Per-track keys are derived, not supplied.
- The Key ID is part of the out-of-band credential. No per-frame header or immutable property carries it.
- The nonce is the identity counter itself, not a salt XOR. The derived key is already unique per credential, physical name, and domain.
- The payload is ciphertext concatenated with the 16-byte tag, with no inner length prefix, encrypted properties, or padding.
- moq-lite frame indices are implied by position in the group ({{moql}} Section "Frame"), not an on-wire object ID. This profile still uses that index as the 32-bit nonce half, because it is the only canonical end-to-end frame identity on both moq-lite and MoQ Transport.
- moq-lite datagrams share the group sequence namespace of the same track ({{moql}} Section "Datagrams"). A grouped frame 0 and a datagram with that sequence would collide under one AES-GCM key, so datagrams use a separate key domain with frame ID zero.

SFrame {{sframe}} is the cryptographic ancestor of {{secure}}.
This profile is not SFrame: it has no SFrame header, no CTR field on the wire, and no per-object KID.

The experimental `moq-secure` format (https://github.com/cathode-ray-tube/moq-secure) is prior art only.
Its independent counter, 17-byte per-frame header, ChaCha20-Poly1305 suite, signing lease, payload padding, and bytes-only processor are not compatibility requirements.


# Profile Version
This document defines profile `moq-e2ee-01`.
A receiver MUST refuse a credential that names any other profile (`unsupported_profile`).
A new profile is a new document; implementations MUST NOT fall back to plaintext or to an older profile because a catalog, announcement, or peer suggested one.


# Credential {#credential}
The application supplies an immutable credential:

~~~
Credential {
  profile (text)
  context (b)
  generation (u64)
  kid (u64)
  secret (32)
}
~~~

**profile**:
The string `moq-e2ee-01`, supplied out of band with the credential and checked before derivation.
It is not carried in protected payloads.

**context**:
Opaque bytes chosen by the application as the broadcast's end-to-end identity.
Both ends MUST use identical bytes.
The MoQ broadcast path is visible to relays and is not this field unless the application copies it in.

**generation**:
A counter the application increments to rotate.
A generation never changes in place.

**kid**:
Selects among credentials the application retains.
It never changes in place.

**secret**:
Exactly 32 bytes from a cryptographically secure random generator.
It MUST NOT be a password, passphrase, or other guessable input.

`generation` and `kid` MUST be in `0..=2^53-1` inclusive, the largest integer TypeScript can represent exactly.
`context` and every `semantic_name` MUST be at most 65535 bytes, the `bytes` encoding width.
An implementation MUST refuse a credential outside those ranges (`identity`) or whose secret is not 32 bytes (`invalid_secret`).

Applications distribute credentials over their own authenticated channel.
MoQ announcements, catalogs, paths, and relay authorization MUST NOT carry the secret or authenticate it.

The application pins the authorized `(profile, generation, kid)`.
A relay-replayed catalog or announcement is never a freshness authority.
Implementations MUST let the application retain more than one credential and select among them; they MUST NOT infer generation or kid from the transport.


# Canonical Encoding {#encoding}
HKDF info fields use unique encodings, not varints.

- `u16` / `u32` / `u64`: unsigned big-endian integers of that width.
- `bytes`: `u16(length) || data`, length at most 65535.
- ASCII labels are the UTF-8 bytes of the quoted string, with no length prefix of their own.

Integers used as group, frame, generation, or kid identities are refused before encoding if they fail {{bounds}}.


# Key Derivation {#derive}
Keys and physical names are derived with HKDF-SHA-256 {{RFC5869}}.
Let `salt` be the ASCII bytes of `"moq-e2ee-01"`.

~~~
prk = HKDF-Extract(salt, secret)
~~~

Physical name material is 16 bytes:

~~~
name_info = "moq-e2ee-01 name"
            || bytes(context)
            || u64(generation)
            || u64(kid)
            || bytes(semantic_name)
physical  = HKDF-Expand(prk, name_info, 16)
~~~

`semantic_name` is the UTF-8 bytes of the application's track name (`catalog.json`, `video`, and so on).
The physical track name is the unpadded base64url encoding of `physical` ({{RFC4648}} Section 5): 22 ASCII characters, which is a valid moq-lite track name.

AEAD keys are 16 bytes, one per physical name and domain:

~~~
key_info = "moq-e2ee-01 key"
           || bytes(context)
           || u64(generation)
           || u64(kid)
           || bytes(physical_name)
           || domain
key      = HKDF-Expand(prk, key_info, 16)
~~~

`domain` is a single byte: `0x00` for grouped frames, `0x01` for datagrams.
`physical_name` here is the 22-character ASCII string, not the raw 16-byte material.

A given `(credential, physical_name, domain)` tuple has one key.
Implementations MUST derive names from semantic names, then keys from the resulting physical names.


# Object Identity {#identity}
A protected object is the tuple `(credential, physical_name, domain, group, frame)`.

## Grouped Frames
A grouped frame uses `domain = 0x00`, the group's sequence as `group`, and the frame index within that group as `frame`.
moq-lite numbers frames from 0 in write order ({{moql}}).
On MoQ Transport, `frame` is the explicit Object ID, never its arrival ordinal.
Publishers supporting both transports MUST assign contiguous Object IDs from zero so that each matches its moq-lite write-order index; relays MUST NOT renumber protected objects.
An Object ID above `2^32-1` MUST be refused as `identity` before encryption or decryption.

## Datagrams
A datagram uses `domain = 0x01`, its 64-bit sequence as `group`, and `frame = 0`.
MoQ Transport has no datagram mapping in this profile; shared vectors cover grouped tracks on both transports and datagrams on moq-lite only.

## Nonce
The 96-bit AES-GCM nonce is:

~~~
nonce = u64(group) || u32(frame)
~~~

AES-GCM's internal block counter is not `frame`.
Implementations MUST call a standard AEAD API {{RFC5116}} with this nonce and an empty AAD.

The empty AAD is deliberate: profile version, context, generation, kid, physical name, and domain are bound by HKDF; group and frame are bound by the nonce.
Rewritten timestamps and mutable routing properties are not authenticated.


# Payload Protection {#payload}
Let `Nt = 16`.
AES-128-GCM encrypts the application bytes with `key`, `nonce`, and empty AAD.
The bytes placed in the MoQ frame or datagram payload are ciphertext concatenated with the 16-byte tag, in the {{RFC5116}} convention.
There is no inner header.

Credential selection is out of band.
Retransmission and cache replay MUST reuse the original ciphertext.
Encrypting different plaintext at an existing identity is `reuse` and MUST be refused.
A publisher restart or replacement that can reset transport sequence numbers MUST start a new generation.

## Plaintext Ceiling
Protected payload length is plaintext length plus `Nt`.
A publisher MUST refuse plaintext that would make the protected payload exceed the transport payload limit for that object (`oversize`), before it touches the network.

The interoperable grouped-frame payload cap matching current moq-net implementations is 32 MiB, so grouped plaintext MUST be at most `32 MiB - 16` bytes.
moq-lite datagram bodies MUST remain at most 1200 bytes including Subscribe ID, Group Sequence, and Timestamp ({{moql}} Section "Datagrams"); datagram plaintext MUST be at most `1200 - header - 16` for the header that object will actually encode.

## Catalogs
Every catalog representation is encrypted under this profile.
Hang {{hang}} `catalog.json` and `catalog.json.z`, and MSF's `catalog` track, are semantic names; authorized clients derive those physical names from the credential, then learn the remaining opaque names from the decrypted catalog.
A Hang rendition-map key in that catalog is the physical name of the track.

If a representation is compressed, compression is applied to the catalog bytes before AEAD and reversed after decryption.
Encrypting then compressing is forbidden: ciphertext does not compress, and the `.z` sibling would leak the uncompressed size ratio.

## Broadcast Path Suffix
Protected broadcasts MAY use an outer `.e2ee` path suffix such as `foo.hang.e2ee`.
The suffix is an untrusted application convention for exclusion and discovery.
It is not a key identifier and MUST NOT be treated as a cryptographic assertion.


# Bounds {#bounds}
Implementations MUST refuse non-integer identities (including NaN and infinities) and identities outside these bounds before encoding or AEAD:

- `group` (grouped sequence or datagram sequence) and `generation` / `kid`: `0..=2^53-1`. Above that is `identity`.
- `frame`: `0..=2^32-1`. `2^32` and above is `identity`.
- AEAD operations with one key: at most `2^24` invocations and at most `2^36` plaintext bytes (`2^32` 16-byte blocks). Exceeding either is `exhausted`.

`2^53-1` is `Number.MAX_SAFE_INTEGER`.
It is the strictest exact integer bound across current TypeScript and Rust implementations.
The 32-bit frame width is the nonce field.
The `2^24` invocation cap is the interoperable AES-GCM record limit from {{aeadlimits}}.
GCM authenticity also depends on total processed blocks, so `2^24` frames at the 32 MiB transport ceiling would be about `2^45` blocks; the `2^36`-byte cap is the matching total-block bound.
Small records hit the invocation cap first; large records hit the byte cap first.


# Failure Behavior {#failure}
E2EE is an explicit per-broadcast mode.
There is no plaintext fallback.

Typed failures:

- `unsupported_profile`: credential names a profile other than `moq-e2ee-01`.
- `invalid_secret`: secret is not 32 bytes.
- `identity`: an integer is outside {{bounds}}, a `bytes` field exceeds 65535, or `domain` is not `0x00`/`0x01`.
- `exhausted`: the next AEAD operation would exceed `2^24` uses of that key or `2^36` plaintext bytes under that key.
- `reuse`: encrypting different bytes at an identity that already produced ciphertext.
- `oversize`: plaintext plus tag exceeds the transport payload limit, or a ciphertext is shorter than `Nt` or larger than that limit.
- `authentication`: AEAD open fails. Relocation across context, generation, kid, physical name, domain, group, or frame is this failure.
- `duplicate`: a receiver has already opened this identity inside its retained window. Operational, not a cryptographic event.
- `pinned_mismatch`: the credential is not the generation or kid the application pinned.

Authentication failure on a grouped track MUST end that track with `authentication`.
Authentication failure on a datagram MUST drop that datagram and emit `authentication`; the track continues.

Receivers MUST suppress duplicates of identities they still retain.
The window MUST be bounded.
This profile RECOMMENDS retaining the current grouped track's frame indices plus the previous group, and a 1024-sequence sliding window for datagrams.
The AEAD identity and generation rules are the security boundary; a relay may still delay, reorder, suppress, or replay ciphertext outside a receiver's window.

A late subscriber MAY start at any group the publisher still holds.
Gaps are not errors.
A receiver MUST NOT require group 0 or any prior identity before opening a later authentic object.


# Test Vectors {#vectors}
Known-answer and negative vectors live in `moq-e2ee-01.json` beside this draft.
Hex strings are octet sequences.
The JSON is authoritative for primitive interop; an implementation of this profile MUST pass every vector.
Each negative row specifies an `operation`, its inputs, and its expected typed error.
Non-finite frame inputs use the strings `NaN`, `Infinity`, and `-Infinity`; group inputs in identity tests are decimal strings.
Implementations whose types cannot represent an invalid input MUST reject it at their input boundary.

The file covers derivation, physical naming, grouped frames, datagrams with concrete header budgets, catalog JSON and raw-DEFLATE payloads, relocation across every identity dimension, unsupported credentials, tag failure, identity bounds, oversize plaintext, and a new generation reusing a transport sequence.

The shared verifier is stateless.
It does not verify `reuse`, `exhausted`, `duplicate`, or `pinned_mismatch`, publisher restart ownership, or failure propagation.
Each language core MUST test those lifecycle requirements, including restart under the same generation, retransmission without encryption, per-key invocation and plaintext-byte accounting, bounded duplicate suppression, and application pinning.
Passing the primitive vectors alone is not profile conformance.


# Security Considerations
Relays, caches, recorders, and control planes are untrusted for content.
Authorized endpoints that hold the broadcast secret are trusted.
Sender authenticity against another endpoint that also holds the secret is not a goal of `moq-e2ee-01`.

A relay can still observe the outer broadcast path, opaque physical names, group and frame structure, timestamps, sizes, and traffic patterns.
Padding and metadata-flow confidentiality are out of scope.

Nonce reuse under one key is catastrophic for AES-GCM.
The profile prevents it by forbidding re-encryption at an identity, separating datagram and grouped domains, requiring a new generation whenever transport sequences can reset, and capping invocations and plaintext bytes per key.

Empty AAD does not weaken the binding: every immutable end-to-end field is in the HKDF info or the nonce.
Timestamps are excluded because relays rewrite them.

Physical names are deterministic functions of the secret.
An attacker without the secret cannot predict them; an attacker with the secret can derive every name, which is intended.


# IANA Considerations
This document requests no registrations.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Grok, an AI assistant by xAI.
