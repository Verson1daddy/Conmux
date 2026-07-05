# Wire Protocol Reference

*(This page is for people who want to **hook up to the daemon from code**, or who want to know exactly what bytes run over the pipe. If you only use the GUI or the CLI, you can skip it.)*

The Conmux daemon and its clients (the CLI, the GUI, your own programs) exchange a stream of JSON frames over a **named pipe**. This page spells out the shape of those frames — every one maps to a real `#[derive(Serialize, Deserialize)]` type in `crates/conmux/src/protocol.rs`; this is not documentation invented from scratch.

> **Stability note**: the protocol layer is the **only** public surface conmux promises to keep stable (the serde shape is the contract; breaking changes go through a minor bump + CHANGELOG). Every API outside the protocol layer is still unstable. Every type name below matches one-to-one against `protocol.rs` / `event.rs`.

## What Runs on a Connection: the `WireFrame` Envelope

Every frame is wrapped in an envelope enum, `WireFrame`, externally tagged (in JSON that's `{"VariantName": {...}}`). There are five kinds:

| Frame | Direction | Payload |
|----|------|------|
| `Hello` | client → daemon | `protocol_version: u32`, `client_kind: String` |
| `HelloAck` | daemon → client | `protocol_version: u32`, `daemon_version: String` |
| `Request` | client → daemon | a `MuxRequest` |
| `Reply` | daemon → client | a `MuxReply` |
| `Notify` | daemon → client | a `MuxNotify` (async event, no correlation id) |

**Directions are enforced, not a free-for-all**: the daemon only accepts `Hello` (and only during the handshake) and `Request`; clients only accept `HelloAck` / `Reply` / `Notify`. Sending a frame the wrong way = protocol error, and the connection is dropped on the spot.

**Handshake first**: after connecting, a client's **first frame must be `Hello`**. `client_kind` is just a free-form label that goes into the audit log — it **plays no part in any authorization decision**, so don't expect to escalate privileges with it.

## Versions Must Match Exactly

`PROTOCOL_VERSION` is currently `1`, and it is **independent of the crate version**. During the handshake the daemon checks its own version against the `protocol_version` in `Hello` for **strict equality** — not "greater than or equal", but "must be identical". If they don't match, you don't get a `HelloAck`. Any breaking change to the wire shape must bump this constant.

## Unknown Fields Are Rejected (`deny_unknown_fields`)

The `WireFrame` envelope, `Hello` / `HelloAck`, and `MuxOp` all enable `deny_unknown_fields`: one extra undefined key in a message and **deserialization fails outright**, instead of being silently ignored. The most important job this rule does is keeping injection sources out at the door — see `Send` below.

## Requests: `MuxRequest` and the `MuxOp` Operation Enum

A request = a correlation id + one operation:

```
MuxRequest { correlation_id: u64, op: MuxOp }
```

`correlation_id` pairs replies with requests (the reply carries the same number back). `MuxOp` is the full set of operations you can issue, listed one by one below. The right-hand column is the `MuxReply::Ok` payload (a `MuxPayload` variant) that operation produces on success.

| `MuxOp` variant | Fields | Success payload (`MuxPayload`) | Notes |
|-------------|------|------------------------|------|
| `Spawn` | `SpawnRequest` | `Spawned(PaneId)` | Spawn a new pane |
| `Respawn` | `SpawnRequest` | `Spawned(PaneId)` | Atomic same-ID restart (closes the ID-reuse window between KillTree+Spawn) |
| `Send` | `pane_id`, `data` (see below) | `Sent` | Inject input into a pane |
| `Capture` | `CaptureRequest` | `Captured(CaptureResult)` | Grab scrollback |
| `Resize` | `pane_id`, `size: PaneSize` | `Resized` | Change a pane's rows/columns |
| `KillTree` | `pane_id` | `Killed` | Kill the entire process tree |
| `ListPanes` | none | `Panes(Vec<PaneState>)` | List all panes |
| `Subscribe` | `pane_id` | `Subscribed` | Subscribe to that pane's event stream |
| `Unsubscribe` | `pane_id` | `Unsubscribed` | Cancel the subscription |
| `Attach` | `pane_id` | `AttachSnapshot { ... }` | Atomic "subscribe + snapshot" |
| `ListThemes` | none | `Themes(Vec<TerminalTheme>)` | List theme presets |
| `SetTheme` | `id: String` | `ThemeSet` | Hot-switch the theme; also broadcasts `ThemeChanged` |
| `KillServer` | none | `ServerKillScheduled` | Terminate the daemon and all sessions |
| `PinExecutable` | `path: String` | `Pinned` | Pin an executable into the trust store |
| `UnpinExecutable` | `path: String` | `Unpinned` | Remove a pin |

That's **all** 15 variants — no more, no fewer. `MuxOp` **deliberately omits** `#[non_exhaustive]`: adding a variant is an explicit minor-version decision, the daemon's dispatcher matches on it exhaustively, and a future new variant will fail to compile on the daemon side — forcing you to handle it explicitly instead of silently missing it.

### About `Send`: the Injection Source Never Crosses the Wire

`Send`'s `data` is **raw bytes**, base64-encoded on the wire (arrow keys, Alt combinations, binary pastes — non-UTF-8 content like that can't be carried losslessly in a plain string).

**Note that `Send` has no `source` field** — that's deliberate: the injection source is assigned at the receiving boundary based on channel identity, and **clients are not allowed to self-report it on the wire**. Combined with `deny_unknown_fields`, a `Send` message carrying a `source` key **fails at deserialization** — rejected, not accepted and then discarded.

### About the Subscription Operations

`Subscribe` / `Unsubscribe` maintain the set of "which panes this connection cares about"; the daemon's fan-out (`FanoutSink`) uses it to deliver `PaneOutput` / `PaneExited` events **only to connections that subscribed**. `Attach` is the one-step "subscribe + atomic snapshot": it registers the subscription first, then takes a snapshot of the current scrollback, and from there feeds the live stream continuously with `seq > last_seq` — no dropped frames, no duplicated frames in between (rate limiting + per-pane concurrency=1 guard against snapshot amplification). This "fan-out delivery + seamless snapshot stitching" is **implemented and tested** in the current daemon (the M2 milestone has landed; see `daemon.rs::attach_with_limits` / `FanoutSink`). (`SetTheme` explicitly does **not persist** the theme preference — persistence belongs to higher-level consumers / the GUI shell, not the daemon.)

## Replies: `MuxReply` and `MuxPayload`

Replies take one of two paths, both carrying a `correlation_id` for pairing:

```
MuxReply::Ok  { correlation_id: u64, payload: MuxPayload }
MuxReply::Err { correlation_id: u64, error:   ConmuxError }
```

Errors go through `Err`, carrying a `ConmuxError` (a **mechanism-layer** error, e.g. `PaneNotFound`) — conmux only reports its own mechanism-layer errors; it doesn't recognize conflux's semantic errors.

The success payload `MuxPayload`'s variants are already mapped one-to-one in the right-hand column of the table above. Most are field-less acknowledgements (`Sent` / `Resized` / `Killed` / `Subscribed` / …). Two carry real data and are worth a closer look:

- **`AttachSnapshot`** — the atomic snapshot for `Attach`. Fields: `mode_preamble_b64` (terminal mode preamble, e.g. alt-screen), `history_b64` (scrollback history), `last_seq` (last sequence number), `pane_state`. Client-side reconstruction = feed the preamble → feed the history → then feed the live stream continuously with `seq > last_seq`, nothing dropped, nothing duplicated.
- **`Captured(CaptureResult)`** — the result of `Capture`, containing the base64 data, first/last absolute line numbers, whether it was truncated, and whether the buffer is "actually full".

`MuxPayload` **does** carry `#[non_exhaustive]` (the opposite of `MuxOp`), so consumers must keep a `_ =>` arm when matching.

## Async Events: `MuxNotify`

Events the daemon pushes to clients on its own initiative, wrapped in `WireFrame::Notify`, with **no correlation id** (they don't answer any request). Defined in `event.rs`; there are three:

| `MuxNotify` variant | Fields | Meaning |
|-----------------|------|------|
| `PaneOutput` | `pane_id`, `seq: u64`, `data` (base64) | The pane's raw output; `seq` is a per-pane monotonic sequence number, for replay reconciliation |
| `PaneExited` | `pane_id`, `exit_code: Option<i32>` | The process exited; when the exit code can't be obtained it's `None`, **never faked** as 0 |
| `ThemeChanged` | `id: String` | Broadcast after the theme is switched via `SetTheme`, for live reskinning |

Same as `Send`, `PaneOutput.data` also travels as base64 on the wire (raw bytes may contain unprintable / non-UTF-8 content). In-proc directly-connected consumers (sink implementations) still receive the raw `Vec<u8>`; base64 only applies at the pipe boundary.

The monotonicity of `seq` comes with a hard rule: if a consumer or conmux coalesces `PaneOutput` frames, it may **only concatenate — never drop bytes, and `seq` must stay contiguous** — dropped frames leave the consumer making wrong decisions on incomplete output.

> **Honest boundary**: `MuxNotify` only emits events conmux **knows for certain at the mechanism layer** — bytes, exits, theme changes. It does **not** emit semantic states like "this agent is thinking / waiting on a permission request": that's an upper layer's (e.g. Conflux's) interpretation of the PTY content, and it doesn't belong to the protocol layer. Don't expect to read agent state directly off the wire.

## Cross-Check at a Glance

To verify all of the above quickly, the `#[cfg(test)]` module at the bottom of `protocol.rs` is living documentation: `all_ops_round_trip` lists all 15 ops, `wire_frame_all_directions_round_trip` exercises the five envelope frames, and `send_with_source_field_is_rejected_on_wire` and `hello_rejects_unknown_fields` demonstrate both places where unknown fields get rejected. Before changing the protocol, make these tests go red first — it's the most reliable way to reconcile.