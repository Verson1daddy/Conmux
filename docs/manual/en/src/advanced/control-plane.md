# Driving conmux from Code

*(This page is for people who want to build automation, write tools, or hook Conmux up to their own scripts/programs. It assumes you know a little Rust; if you don't write code and just want to drive things with keystrokes, feel free to skip this page.)*

## One sentence up front

With most terminal multiplexers, the only way to poke them is the keyboard. Conmux is different: **behind it sits a control plane you can drive directly from code** — opening sessions, sending keystrokes, capturing screens, following a pane's output as it streams by — all of it can be done programmatically, not just via shortcuts.

## The skeleton: one daemon + a bunch of thin clients

Conmux uses the tmux-style "server" model:

- **The daemon holds all the real ConPTYs.** Every pane you open — the process, its entire child-process tree, the scrollback history, the terminal mode state — lives inside this one daemon.
- **Clients are "thin"** — whether it's the `conmux` command line, the GUI shell, or a program you wrote yourself, each one just connects to the daemon over a **named pipe** and talks to it. Clients themselves hold no panes.

This is why "closing a client window doesn't kill what's inside": the only thing that dies is that connection — the pane keeps running in the daemon. (The flip side: only when the daemon itself goes away — `kill-server` or a crash — do all panes go with it. That's the other half of the "zero orphan processes" guarantee; see [Mechanism vs. Semantics: Where the Boundary Is](../authors/boundary.md).)

> **Trust boundary**: the pipe is open to the **current user** only (the DACL grants access solely to your own SID, and remote clients are rejected). The control plane is **local** — no accounts, no telemetry, nothing leaves this machine.

## The control plane gives you three things

Verified against the source: these three things the `conmux` crate exposes are the core of the control plane:

1. **Request / reply** — a framed, one-question-one-answer protocol. You send an operation (`MuxOp`), the daemon sends back a reply (`MuxReply::Ok { payload }` or `Err`), with a `correlation_id` so the two can be matched up.
2. **A per-pane event stream** — subscribe to a pane and you'll receive its `PaneOutput` (with a `seq` number that is **strictly monotonic per pane, starting from 1**) and `PaneExited` (with the exact exit code).
3. **Stable pane ids** — every pane has a stable `PaneId`, and that's how you address it: send, capture, resize, attach all go by this id.

With these three, you can **drive it from code instead of just keystrokes**.

## Driving it from Rust

The client entry point is `conmux::client::Client` (all the method names below have been verified against `client.rs`):

```rust
use conmux::client::Client;
use conmux::protocol::MuxOp;

// Connect to the current user's daemon; if there is no daemon, one is spawned for you automatically (the tmux mindset).
let mut client = Client::connect_or_spawn()?;

// Request/reply: list all current panes.
let panes = client.request(MuxOp::ListPanes)?;

// Inject keystrokes into a pane (raw bytes, going through the single audited write path inside the daemon).
client.request(MuxOp::Send { pane_id: id.clone(), data: b"ls\r".to_vec() })?;

// Capture a pane's current screen (optionally with/without ANSI).
let snap = client.request(MuxOp::Capture(cap_req))?;
```

To **follow a pane's output continuously** (rather than just grabbing a snapshot), use `attach` — it first hands you an atomic snapshot (terminal-mode preamble + scrollback history + sequence high-water mark), then turns into a streaming session where you loop on live output and can also inject stdin back:

```rust
let attached = client.attach(&id)?;
// First feed the renderer in mode_preamble → history → buffered order to rebuild the screen
let mut session = attached.session;
while let Some(ev) = session.recv_output() {
    // AttachEvent::Output { seq, data } / Exited { exit_code }
}
```

Other common operations (all variants of `MuxOp`, verified against `protocol.rs`): `Spawn` / `Respawn` (atomic restart under the same id) / `Resize` / `KillTree` / `Subscribe` · `Unsubscribe` / `ListThemes` · `SetTheme` / `KillServer`. For the full list and the shape of every payload, see the next page: [Wire Protocol Reference](protocol.md).

## Honest boundaries: what's stable, what will change

Conmux's stability promise comes in **two tiers**, and this matters — verified against the `crate::lib` docs:

- **✅ The protocol layer is a frozen contract (committed).** The wire protocol types (`MuxRequest` / `MuxOp` / `MuxPayload` / `MuxReply` / `MuxNotify`, plus the closure of types they carry, such as `PaneId` / `PaneSize` / `PaneState`), the `PaneHost` facade, the event surface, the injection extension points, the theme surface — during 0.x, any change to these **must go through a minor version bump + CHANGELOG**; patch releases will not break you. The sequence semantics (`PaneOutput.seq` strictly monotonic per pane, starting from 1) are frozen alongside them. **Building automation against the protocol layer is safe.**
- **⚠️ APIs outside the protocol layer are unstable and may change without warning before 1.0.** Concretely: the `daemon`, `client`, `pipe`, and `wire` modules are all explicitly marked "Stability: unstable — may change without notice" in the source. In other words, the **specific Rust method shapes** above — `Client::connect_or_spawn()` / `attach()` — belong to the will-change tier: they work, Conflux itself uses them for real, but don't treat them as a frozen contract. If you want a long-term anchor, anchor on the **protocol itself** (the wire types), not on any particular client method signature.

> The one-line rule of thumb: **the protocol is the contract; the client API is the current implementation**. When building automation, the closer you stay to the wire protocol, the harder it is for a future release to knock you over.

## What's next

- Want the full list of operations, the fields on every request/reply, and what event frames look like? → [Wire Protocol Reference](protocol.md)
- Want to bring in your own agent framework? → [Hooking Your CLI Up to conmux](../authors/onboarding.md)