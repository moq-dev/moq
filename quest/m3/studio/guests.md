# [M] Studio guests

## Goal

The studio owns a moq-room prefix and shows a join link and QR code. A guest
or a phone opens a demo/web page (js/publish), and each participant's camera
and screen appear in the studio as sources. The host sees who joined and can
remove a participant from the scene.

## Plan

- **Rooms:** reuse moq-room's conventions (`{identity}/camera.hang`,
  `{identity}/screen.hang`). Do not invent a guest protocol.
- **Tokens:**
  - Minting a moq-auth token needs a signing key or an issuer API. For a
    self-hosted relay, the studio accepts only a key whose moq-auth `Scope`
    is rooted at its room prefix, and refuses an unscoped relay-wide key. A
    stolen workstation key then cannot sign for other tenants. A hosted
    issuer brings its own API.
  - Each guest token is short-lived and minted with `moq_room::claims(room,
    identity)`, so it publishes only under that guest's own `<identity>/`.
    A guest cannot impersonate another participant.
- **Join link:** carry the token in the URL fragment, so it never reaches a
  server log.
- **Relay required:** guests join through a relay. Peer-to-peer WHIP ingress
  inside the studio was considered and dropped, because it brings NAT
  traversal and TURN for no capability the relay lacks.
