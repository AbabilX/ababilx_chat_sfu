# ferrite-sfu

[![Deploy SFU](https://github.com/AbabilX/ababilx_chat_sfu/actions/workflows/deploy.yml/badge.svg)](https://github.com/AbabilX/ababilx_chat_sfu/actions/workflows/deploy.yml)
[![Licence: MIT OR Apache-2.0](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue.svg)](#licence)

A small WebRTC SFU for **group audio and screen sharing**, written in Rust on
[str0m](https://github.com/algesten/str0m).

It forwards RTP and nothing else. The payload is never parsed, decoded, or
re-encoded — everything the server needs in order to route (audio level,
keyframe requests, congestion feedback) travels in RTP headers and RTCP. That
is a deliberate design constraint, not an omission: it means an application can
add end-to-end encryption as a **client-only** change, and the server keeps
working without ever being able to listen.

Not a LiveKit replacement. There is no recording, no transcoding, no simulcast
selection, no cascading, no SIP. If you need those, use LiveKit or mediasoup.

It is deliberately standalone: nothing in here knows about the application that
happens to run it. The only coupling is a shared HS256 secret and a webhook
shape, both described under [Integration contract](#integration-contract).

## What it does

- One process, one UDP port, many rooms.
- Group audio: every participant's Opus stream forwarded to everyone else.
- Screen share: one video stream per publisher, keyframes pulled on subscribe.
- Active-speaker detection from the RFC 6464 audio-level header extension,
  computed **without touching the payload**.
- Room lifecycle reported to a host application over a signed webhook.
- `/diag`, a self-test page that opens a websocket back to the server and shows
  the result — for when a client fails and there is no console to read.

## Running

```sh
cp .env.example .env      # set SFU_SHARED_SECRET and SFU_PUBLIC_IP
cargo run --release
```

Or:

```sh
SFU_SHARED_SECRET=local-dev-secret docker compose up --build -d
```

## Configuration

`SFU_PUBLIC_IP` is not optional on a server. It is the address put into the ICE
host candidate; auto-detection picks the machine's own interface, which is the
wrong answer behind any NAT or inside any container. Getting it wrong produces
the worst possible symptom: signaling succeeds, the join looks fine, and no
audio ever arrives.

| Variable | Default | Meaning |
| --- | --- | --- |
| `SFU_BIND_HTTP` | `0.0.0.0:7898` | Signaling WebSocket + `/healthz` + `/diag` |
| `SFU_BIND_UDP` | `0.0.0.0:7899` | All media, all rooms |
| `SFU_PUBLIC_IP` | auto-detected | Advertised ICE host candidate |
| `SFU_SHARED_SECRET` | *required* | HS256 key for join tokens |
| `SFU_WEBHOOK_URL` | unset | Where lifecycle events are POSTed |
| `SFU_WEBHOOK_SECRET` | `SFU_SHARED_SECRET` | HMAC key for those events |
| `SFU_ICE_LITE` | `false` | Enable when publicly routable |
| `SFU_MAX_ROOM_PEERS` | `16` | Refuses joins beyond this |

`SFU_SHARED_SECRET` is the only thing standing between a stranger and your
rooms. Generate it with `openssl rand -hex 32`, give it to nothing but the
service that mints tokens, and never commit it.

## Deployment

Two ports, and they are **not** deployed the same way.

```
browser ──── wss:// ────► reverse proxy (TLS) ────► :7898  signaling
        ──── UDP  ──────────────────────────────► :7899  media
```

**Signaling goes through your proxy.** A page served over HTTPS cannot open a
`ws://` socket — the browser blocks it as mixed content, with a failure that
looks exactly like the server being down. Terminate TLS and hand clients a
`wss://` URL.

```nginx
location /ws {
    proxy_pass http://127.0.0.1:7898;
    proxy_http_version 1.1;
    proxy_set_header Upgrade    $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 1h;      # a call is one long-lived socket
}
```

**Media must not.** UDP 7899 is published straight on the host and opened in
the firewall. Do not route it through an HTTP proxy, and on Docker Swarm
publish it in **host mode** — the ingress routing mesh rewrites the source
address, and the SFU learns where to send media from the source address of the
first STUN packet it receives.

```yaml
ports:
  - target: 7899
    published: 7899
    protocol: udp
    mode: host          # NOT ingress
```

```sh
ufw allow 7899/udp
```

Set `SFU_ICE_LITE=true` once the box has a routable public address and nothing
translating it; the SFU then answers connectivity checks instead of also
sending its own.

`/healthz` is a liveness probe that deliberately reports nothing about rooms —
a call in trouble must not make an orchestrator restart the process and drop
every other call with it. The container image ships its own healthcheck and
needs no shell.

**Verify from outside.** Open `/diag` in a browser: it loads over HTTP, then
opens a real WebSocket back and puts the result on the page, which is the
quickest way to tell "the service is down" apart from "this network blocks
it". Then point the conformance suite at the deployment (below) — that is the
only check that proves the advertised candidate and the published UDP port
actually carry media.

## Testing

`cargo test` runs a conformance suite that stands up the SFU in-process and
drives it with peers built out of str0m — real signaling, SDP, ICE, DTLS, SRTP
and forwarding, with no browser involved.

The same suite can be pointed at a deployment that is already running, which is
the only way to check that the advertised ICE candidate and the published UDP
port actually work end to end:

```sh
FERRITE_SFU_TEST_URL=ws://127.0.0.1:7898 \
FERRITE_SFU_TEST_SECRET=local-dev-secret \
  cargo test --test forwarding -- --test-threads=1
```

## Integration contract

### 1. Mint a join token

HS256 JWT signed with `SFU_SHARED_SECRET`. Keep the TTL short — 90 seconds is
plenty, since it is spent immediately.

```json
{
  "sub": "user-uuid",       // participant identity, echoed in webhooks
  "room": "room-name",      // rooms are created by the first join
  "call_id": "call-uuid",   // opaque; carried for your own correlation
  "name": "Ada",
  "avatar_url": "https://…",
  "exp": 1700000090
}
```

### 2. Signal over `GET /ws`

JSON frames. **The first frame must be `join`.** After that the SFU is the only
side that offers, so there is no glare to resolve and the client never has to
implement rollback.

Client to server:

| Frame | Meaning |
| --- | --- |
| `{"t":"join","token":"…","sdp":"…"}` | Offer one audio m-line (mic) and one video m-line (screen), both `sendonly` |
| `{"t":"answer","sdp":"…"}` | Answer to an SFU offer |
| `{"t":"mute","muted":true}` | Advertise mute to the room |
| `{"t":"screen","active":true}` | Advertise screen sharing; drives the webhook |
| `{"t":"leave"}` / `{"t":"ping"}` | |

Server to client:

| Frame | Meaning |
| --- | --- |
| `{"t":"joined","peer_id","call_id","sdp","peers":[…]}` | Answer to the join offer, plus the current roster |
| `{"t":"offer","sdp","tracks":[{"mid","peer_id","kind"}]}` | Renegotiation. `tracks` says whose media each m-line carries — SDP alone does not |
| `{"t":"peer_joined"}` / `{"t":"peer_left"}` / `{"t":"peer_state"}` | Roster changes |
| `{"t":"speaking","peer_ids":[…]}` | Current speakers |
| `{"t":"error","code","message"}` / `{"t":"pong"}` | |

Start the screen share with `replaceTrack` on the video sender that was
negotiated at join time. It needs no renegotiation, which is why the m-line is
offered up front even with no track attached.

### 3. Receive lifecycle webhooks

`POST` to `SFU_WEBHOOK_URL` with header `x-sfu-signature`, a base64
HMAC-SHA256 of the **raw request body** keyed by `SFU_WEBHOOK_SECRET`.

```json
{
  "id": "uuid",
  "event": "participant_joined",
  "room": { "name": "room-name" },
  "participant": { "identity": "user-uuid" },
  "track": { "source": "SCREEN_SHARE" }
}
```

Events: `participant_joined` (on ICE connect, not on socket open — an arrival
means media can actually flow), `participant_left`, `track_published`,
`track_unpublished` (both screen-share only), `room_finished`, and
`sfu_started`.

`sfu_started` is announced once on boot with an empty room name. It is not a
room event: it says every room this process was carrying is gone, because
WebRTC state lives only in memory and a restart genuinely ends those calls.
Handle it by ending them, or participants stay marked as being in a call that
no longer exists. It is retried harder than the others for that reason.

The field names deliberately match LiveKit's webhook payload, so an existing
LiveKit integration can point here and keep its handler, its de-duplication and
its billing untouched. Delivery is at-least-once and retried; **de-duplicate on
`id`**.

## Design notes

- **One thread owns all media.** str0m is sans-I/O and synchronous, so it gets
  its own OS thread; signaling runs on Tokio and talks to it over a channel.
  There are no locks on the packet path. Sharding rooms across threads is the
  obvious next step and nothing in the design blocks it.
- **One UDP port for everything.** Demultiplexing is str0m's `accepts()`, which
  knows which instance owns an ICE ufrag or a DTLS association.
- **A publisher leaving is self-healing.** Subscribers hold a `Weak` to the
  publisher's track; the upgrade fails and the m-line is stopped on the next
  negotiation.
- **Reconnecting replaces, it does not duplicate.** A second join from the same
  identity retires the previous peer without reporting a departure, so a host
  application keyed on identity is not confused by the overlap.

## Limitations

No simulcast or SVC layer selection. No TURN — deploy coturn if you need to
reach clients on UDP-blocked networks. No recording or server-side mixing, and
with end-to-end encrypted payloads there cannot be. IPv4-focused. One process,
no clustering.

## Contributing

Issues and pull requests are welcome. `cargo clippy --all-targets -- -D warnings`
and `cargo test` both have to pass; CI runs them before anything is published.

Please do not put a real `SFU_SHARED_SECRET`, public address, or webhook URL in
a patch — `.env` is ignored, and `.env.example` is the place for placeholders.

## Licence

Dual licensed under either of

- MIT ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option. Unless you state otherwise, any contribution you intentionally
submit for inclusion is dual licensed as above, with no additional terms.
# ababilx_chat_sfu
