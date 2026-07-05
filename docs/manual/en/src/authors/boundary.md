# Mechanism vs. Semantics: Where the Boundary Is

If you're building your own agent framework and want to bring conmux in, this is the page to read first. It's not about any particular API — it's about a **deliberately hard-drawn boundary**. Understand it, and you'll understand why conmux is worth using as a foundation, and which jobs it **deliberately refuses to do for you**.

## In one sentence

**conmux only knows about panes, processes, and bytes. It has no idea what an "agent" is.**

All those concepts you've heard elsewhere — which agent is waiting on you, who should move to the front of the attention queue, how multiple agents collaborate, which light should turn on in the dynamic island — conmux recognizes **none of them**. To conmux, whether a pane is running Claude Code, `cargo build`, or a plain PowerShell makes no difference whatsoever: each is a supervised process tree that emits bytes and occasionally needs you to feed it input.

This isn't "not built yet." It's **deliberately not built**.

## Why draw this line on purpose

This is exactly what separates a "foundation" from a "framework."

A **framework** nails the semantics down for you: it assumes you're building agents, assumes how agents should queue, how they should talk to each other — going along with its assumptions is convenient, but the moment your ideas diverge from its, you're fighting it.

A **foundation** provides only mechanism and leaves the semantics blank: it gives you solid ground (processes don't run wild, input gets audited, dropped connections can be reattached), and as for what you build on top — an agent workbench, a CI dashboard, a multi-session ops tool — it doesn't decide for you, and therefore never gets in your way.

conmux chose the latter. It takes the dirty, hard job of "how a CLI session gets reliably hosted, supervised, and reconnected after a disconnect" and does it all the way down, then **stops right where semantics begin**. The sentence "this pane is an agent waiting on a permission request" is one conmux cannot say — because "agent" and "waiting on a permission" are **upper-layer words**, to be defined by the consumer.

> This is also why, in conmux's public API, you can search all you want and never find a single type with `Agent` in its name. What's exposed is `PaneHost`, protocol messages (`MuxRequest` / `MuxReply` / …), event outlets (`PaneOutput` / `PaneExited`), injection hooks, and themes — all vocabulary from the "pane / process / bytes" layer. (For the concrete type names, see [Driving conmux from Code](../advanced/control-plane.md).)

## So who does the semantics — you

The other half of the boundary is the space left for you. **The semantic layer — "what an agent is, how multiple agents collaborate" — conmux never touches; it's handed entirely to the layer above.** Conflux is exactly such a layer: on top of the panes / events / injection that conmux provides, it defines its own concepts like "this pane is an agent," "this agent popped a permission request and the user should be alerted," and "broadcast this user message to these agents."

In your case, the two sides of the boundary divide roughly like this:

| conmux handles (mechanism) | You handle (semantics) |
|---|---|
| Spawn the process, supervise the whole process tree, hand you an accurate exit code when it dies | Decide "this pane represents an agent / a build / a service" |
| Deliver the bytes a pane emits to you in order (`PaneOutput`) | Read from the bytes that "it's waiting for my input," "it errored," "it finished" |
| Provide the single, auditable input channel and deliver the bytes you feed into the PTY | Decide "when to feed, what to feed, whether it should pass a policy check first" |
| Reattach the screen exactly as it was on reconnect | Decide "what the UI looks like, who goes first in the attention queue" |

In one sentence: **conmux guarantees the ground beneath is flat, stable, and watchable; what gets built on it is up to you.**

## The dependency is one-way (please keep it that way)

This boundary is a hard constraint in the code, not a verbal agreement: the dependency direction is always **`your framework → conmux`**, never the reverse. conmux **depends on no upper layer** — it doesn't depend on Tauri, isn't aware of any UI, and certainly doesn't recognize any specific framework's business concepts.

The benefit to you is direct:

- Swap out the entire upper layer (a different UI, a different interaction model, even a different language to drive it), and the conmux ground beneath doesn't have to move.
- When conmux ships a new version, what it promises to keep stable is **only that small, well-defined public surface** (the protocol types + a few core traits); everything else in the internals is explicitly marked "unstable, may change at any time." Write against the public surface, and upgrades are far less likely to be rattled by internal refactors. conmux calls this principle "the library is the product" — a small, stable public surface beats a big all-inclusive one.

## The one thing to remember

> conmux gives you **flat, stable, watchable mechanism**; the **semantics** — "is this an agent, how should they collaborate" — are your job, and your freedom.

This boundary isn't a missing feature; it's design restraint — precisely because it stops where semantics begin, you have room to build your own tower.

---

**Next up**

- Want to see how to drive it from code (real type names, what requests/events look like) → [Driving conmux from Code](../advanced/control-plane.md)
- Ready to hook your CLI up → [Hooking Your CLI Up to conmux](onboarding.md)
