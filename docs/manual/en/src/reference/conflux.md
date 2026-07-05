# About Conflux

Conmux is the foundation; **Conflux is the building on top of it**.

If conmux answers "how do multiple command-line sessions run on Windows, and stay watchable," Conflux tackles something higher-level and more concrete: **you have three or four AI coding agents running at once — how do you keep from drowning in them?**

> In one sentence: **conmux manages panes and processes; Conflux manages agents.** conmux has no idea what an "agent" is — it only knows panes, processes, bytes. Semantics like "this is a Claude Code session and it's waiting for you to approve a permission" are Conflux's job.

## The problem it wants to solve

You run agent CLIs like Claude Code, Codex, Aider, and OpenCode on Windows. One runs fine; three or four at once become unwatchable — you can't stare at four terminals simultaneously, flipping through each one to figure out who's stuck, who's waiting for you to click confirm, who's already finished.

Conflux's design goal is to make sure you **don't have to babysit each one**: surface the one that needs you right now, let you jump back with one click to the moment it asked its question, and record everything that happened while you weren't looking into an audit trail.

It is **not yet another chat window**. Agent work here is treated as real sessions, cards on a canvas, attention signals, permission requests, and a replayable event timeline.

## What it looks like, what it can do

Conflux is a **Tauri 2 + React Windows desktop workbench** running on the conmux kernel. The core boils down to a few things:

- **Multiple agents, one canvas** — open multiple real CLI agents at once, each a live PTY session (ConPTY + xterm.js rendering, full ANSI/colors), laid out as cards on the same canvas — arrangeable, expandable, collapsible. **Not fake cards, not read-only mockups.**
- **An attention surface that comes to you** — dynamic island + sidebar + system tray, delivering each agent's status right to your eyes. For Claude Code sessions, "this one is waiting for you" comes from an authoritative **hook** signal, not screen-scraping guesses — so it's a real event, not an estimate.
- **One-click jump back to the trigger point (jump-back)** — click a notification and Conflux takes you straight to that split pane, straight to the spot that triggered it, no digging through a pile of terminals.
- **Intervene without breaking flow** — expand any card into a two-way terminal, type directly into that agent's session, then collapse it back into the grid when you're done.
- **The gate stays in your hands (permission approval)** — agents' permission requests surface as an approval UI; nothing proceeds without your sign-off.
- **Everything leaves a trace** — every event is written to local **SQLite**, and session timelines can be replayed after the fact.

## Straight talk: the core that works, and the ideas still being shaped

This is the part of the page that most needs to be said clearly. What **actually works and is stable** in Conflux is this one chain:

> **Run multiple real CLI agents → observe every session → attention signals surface what needs handling → you approve / jump back with one click → everything written to SQLite audit, replayable.**

That "supervision console" chain is real. But **how agents collaborate with each other, how they get orchestrated — that set of ideas is still being shaped**. Here's what is **not built** right now, stated plainly:

- **The discussion panel is one-way broadcast, not collaboration.** It does exactly one thing: take one prompt from you and **broadcast it once** to multiple agents.
- **Agents do not talk to each other.** They will **not** funnel their replies into a shared chatroom, will **not** converse with one another — after the broadcast goes out, each runs on its own.
- **There is no automatic orchestration engine.** There is no scheduling brain that "decides on its own who should do what." **The decision-making stays with you** — Conflux supervises agents, but it does not orchestrate them for you.

In other words: **the supervision side is genuinely wired through; the collaboration / orchestration side is still a direction, not the current state.** Don't mistake the roadmap for shipped features.

> The attention surface consists of the **two-state dynamic island + sidebar + system tray**; the floating ball (Float Ball) from early designs has been removed and is not part of it.

## Current status

- **V1 · Windows only · early.** It is a **usable workbench**, not a finished product.
- Runs directly on **ConPTY**, **no WSL dependency**, no Unix-like compatibility layer.
- **No prebuilt / signed installer yet** — for now, please **build from source** (`git clone` → `npm install` + `npm run tauri:build` under `conflux-app`). The installer, once shipped, will be **unsigned** — there's no budget for a code-signing certificate, so SmartScreen will warn "unknown publisher"; click "More info → Run anyway".

Still moving forward on the roadmap (roadmap, not current state): prebuilt / signed installers, broader adapter coverage with simpler configuration, hardening of the attention and permission layers, timeline / audit experience polish.

## How Conflux and Conmux relate

- **`conmux`** — a standalone Rust crate: Windows terminal multiplexing + agent-isolated runtime (ConPTY, whole-process-tree supervision, a single audited input path…), **no Tauri dependency**. It's the star of this manual, and it works on its own.
- **`conflux-app`** — the Tauri 2 + React product layer built on top of `conmux` (canvas, attention surface, permission UI, audit / timeline).

So the manual you're reading right now is about the **foundation, conmux**; Conflux is currently that foundation's **largest consumer**, and a living example of why the "mechanism vs. semantics" boundary deserves to exist. To dig into Conflux itself, head to its project repository.

---

*Conflux and Conmux are both open-source projects built by a student at **South China Normal University**, developed in the open. Got ideas or hit a bug? Head to the corresponding repo to file an issue / open a discussion / send a PR — let's make the agent ecosystem on Windows better together.*
