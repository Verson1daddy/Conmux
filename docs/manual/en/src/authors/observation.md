# What conmux Observes

If you're building your own agent framework, the question you probably care about most is this: **once I bring it into conmux, how much can conmux watch on my behalf?** This page answers that honestly — which signals exist for **any** CLI, which ones only Claude Code gets, and one bottom line that never changes: conmux only observes what a program **actually prints and actually writes to disk**. When it can't get a value it shows "—"; it never makes up a number to look good.

One big premise first. conmux's observation runs inside the **terminal window's graphical shell** (conmux-app), where an "observer" hangs alongside the terminal renderer and subscribes to the same PTY output — it doesn't modify your program, doesn't inject code, doesn't guess what you're up to. It just **reads**.

## What Any CLI Gets: PTY-Level Signals

Whether you hook up PowerShell, a build script, or someone else's agent CLI — as long as it's a normal command-line process, conmux gives you these:

- **Activity (running / idle)** — output within the last short window means `running`; silence for longer than about 2.5 seconds flips it to `idle`. This isn't a judgment call like "it's thinking" — it's literally "has it printed anything to the terminal recently".
- **Process exit + exit code** — when the process ends, conmux gets the exact exit code (the kernel-level `PaneExited` event carries the real exit code, not an estimate). That session's status in the window changes to `exited` accordingly.
- **Terminal bell → attention** — if the program prints a bell character to the terminal (BEL, `\x07`), or the process exits, conmux marks that session as "worth a look". Both are **real signals** (the bell follows tmux's `monitor-bell` semantics), not heuristic guesswork. Switch to that session and take a look, and the mark clears — once you've seen it, it stops bothering you.

That's it. For non-Claude CLIs, conmux provides **only** this layer. It won't try to parse your program's model name, token counts, or internal state — it has no honest way of knowing them, so it simply doesn't make them up. Those fields in the observation state (model / tokens / activity) are always `null` for a plain CLI, and the UI shows "—".

## Claude Code Only: Deep Observation

Besides printing to the terminal, Claude Code also **writes the whole session to disk in structured form** — a JSONL file — and can be configured with a notification hook. conmux understands both, so it can see much deeper into Claude sessions. **This is Claude-exclusive** — it relies on real data that Claude Code itself writes out. Other CLIs don't produce these on-disk artifacts, so they don't get this layer.

How does conmux recognize a Claude session? Either you tell it at launch (the launch command contains `claude`), or it sniffs Claude's stable markers from the terminal output (say, the terminal title being fixed at `✳ Claude Code`, or that `Using Opus 4.8 (1M context)` line). Only once it's identified does conmux **lazily start** the sources below — non-Claude sessions never touch them.

**From reading the JSONL, you get:**

- **Model name (model)** — taken directly from `message.model` in the session transcript (e.g. `claude-opus-4-8`). It's the literal ground truth, not scraped from the terminal.
- **Context usage (contextPct)** — computed from the `usage` of the last real assistant message: `input + cache_read + cache_creation` divided by that model's context window (1M for Opus / Sonnet, 200K for Haiku; if the model isn't recognized, no guessing — it shows "—").
- **Cumulative session tokens (Σ input / output)** — the usage of all real messages across the session, summed up.
- **Subagent tree** — when Claude dispatches subagents, it leaves `tool_use` entries in the transcript (`Agent`, or `Task` in older versions) with a `subagent_type` and a description; when the matching `tool_result` comes back, that subagent is marked done. conmux renders this as a **single flat level** of subagents (Claude's main agent → subagents is exactly one level; it doesn't fabricate deeper nesting).
- **Running workflow / recently invoked skill** — read from the `tool_use` entries and labeled as-is.

> **Honest boundaries (a few gotchas, all documented in source comments)**: ① These fields are only written once the session has seen a **real** assistant message (with real usage — not a `<synthetic>` empty message produced by an interruption); otherwise they stay at "—" rather than fobbing you off with a fake 0. ② These fields come from the JSONL — most Claude versions do **not** print token counts to the terminal, so don't expect to scrape them from terminal text; the deep data's source is that on-disk file. ③ Reading the JSONL requires knowing the session's working directory (cwd) first; until the cwd is available, deep observation **explicitly** tells the UI "can't read this yet" instead of silently showing "—" forever and leaving you baffled.

**The Notification hook, driving more accurate attention:**

Claude Code's Notification hook can signal at two moments — **a permission request prompt pops up** (`permission_prompt`) and **Claude asks you something while idle** (`idle_prompt`). conmux takes these events in, and when the type is on the registered list, it marks the session as needing attention. This tracks reality better than a plain terminal bell — it lines up precisely with the real moment when "Claude is stuck waiting for your approval / your answer".

One more piece of honest labeling around the hook: conmux only lights up "deep awareness active" after it has **actually received** a hook event and confirmed that pipeline works. It does **not** make the reverse claim that "unlit means the hook is broken" — maybe this session just hasn't triggered a permission prompt or an idle question yet. Positive labeling only, no negative inference.

## The One-Line Wrap-Up

- **Any CLI**: activity, exit codes, and bell- / exit-triggered attention. Enough for you to keep a whole row of sessions under watch.
- **Claude Code**: a layer of structured deep observation stacked on top — model, context, tokens, the subagent tree, plus hook-driven precise attention.
- **The bottom line stands**: observation = reading what it **actually prints and actually writes to disk**. When it can't read something, it honestly shows "—" — never speculating, never filling in fake numbers.

To bring **your own** agent CLI in, get it at least the PTY-level observation, and even leave hooks for conmux to recognize it, see [Hooking Your CLI Up to conmux](onboarding.md).