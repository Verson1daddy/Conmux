# Hooking Your CLI Up to conmux

*(This page is for people building agent frameworks / CLI tools. You have a command-line program of your own and want it to run inside conmux — and be watched over.)*

## First, a Straight Answer

**The "one-click onboarding for any framework, auto-detection, auto-adaptation" scaffolding — conmux doesn't have that yet.** Don't let anyone's pitch tell you otherwise. What conmux can give you today comes in two tiers, with a clear line between them:

1. **Any CLI can be launched as a supervised pane** — this is the foundation that **already works**. Your program, someone else's program, PowerShell, tools inside WSL — all the same.
2. **A layer of deep observation for Claude Code sessions** — this is **currently the only** framework with a dedicated adapter.

As for "you toss in any agent framework and conmux automatically recognizes what it is, automatically parses out its model / tokens / subtasks" — **that's the direction, not the current state**. To get that layer, someone has to write a dedicated adapter per framework, the way it was done for Claude. So this page comes in two parts: first the tier that holds for everyone, then the Claude-only tier.

## Tier 1: Launch Any CLI as a Supervised Pane (Works for Everyone)

conmux doesn't care whether your program "is an agent" — it only knows **processes, bytes, panes**. To hook up your CLI, all the information you need fits in one command:

- **program** — the executable to launch (e.g. `my-agent.exe`, `node`, `python`).
- **args** — launch arguments (e.g. `--serve`, `run main.py`).
- **cwd** — working directory (optional; if omitted, the current directory is inherited).

In the GUI, these three together form a **launch entry**: a display name + one line of raw command text (including arguments) + an optional working directory. Two runnable entries come built in — `Shell` (`powershell.exe`) and `WSL` (`wsl`) — and you can add your own alongside them, edit them, delete them. The raw command text is split into program and args by "whitespace tokenization + double quotes to contain segments with spaces" (pipes, redirection, and variable expansion are **not supported** — those are beyond what a launch entry covers).

Here's what happens when you press launch: conmux **starts your command directly as a real ConPTY process** (not by pouring characters into some shell's stdin), so your CLI becomes a supervised pane. From that moment on, everything the earlier chapters described **holds for it in full**:

- **Split panes** side by side with other panes, and **switch** between a row of session dots;
- Close the client window and your process **doesn't die**; re-attach and the screen picks up exactly where it was (detach / attach);
- It — together with all the child and grandchild processes it forks — is held by a Job Object: **when it's time to kill, the whole tree goes down clean, no orphans left behind**;
- Every byte of input written to it goes through the same audited channel.

**This tier doesn't discriminate by framework.** As long as your agent CLI can run in a Windows command line, it can become a conmux pane and get the entire package above. This is the most solid part of what conmux can offer "any framework" today.

> **Want a more precise onboarding contract?** How program / args / cwd flow into spawn, how launch entries are persisted, how commands are parsed — the GUI-side source of truth is `conmux-app/src/lib/launch-registry.ts` and `sessions.ts`. For driving the kernel from code (framed protocol, stable pane ids, event stream), see [Driving conmux from Code](../advanced/control-plane.md).

## Tier 2: Deep Observation for Claude Code (Currently the Only Dedicated Adapter)

Beyond "launching as a pane", conmux does one more layer for **Claude Code** sessions — it can tell which model the session is running, how much context has been used, which subtasks were dispatched, and when it's waiting on you. This layer **was written specifically for Claude**; it's not a generic capability.

When you launch with a **bare `claude`** command (literally just `claude`, no arguments of your own) through conmux, conmux automatically does three things:

1. **Injects `--session-id <uuid>`** — pins a unique id on the session, which lets the observation side **anchor precisely** onto its log file, with no cross-talk from other historical sessions in the same directory.
2. **Reads along the JSONL** — Claude Code writes session content as JSONL logs; conmux reads them incrementally, parsing out structured information like model name, tokens, context usage percentage, and the sub-agent tree.
3. **Attaches a Notification hook** — by temporarily writing a `--settings` file (effective only for this session, and **merged with, not overriding**, your own global hooks), it picks up structured "needs you" signals like "waiting for you to approve a permission request / idle waiting for your input", covering the TUI permission-dialog cases that a terminal bell (BEL) can't catch.

A few boundaries, stated up front:

- **Only applies to a bare `claude`.** The moment you pass **any** explicit argument (even your own `--session-id`, `-c`, `-p`), conmux **won't touch a single character of your command** — no injection, no hook. This is deliberate discipline: don't overstep and rewrite someone's launch.
- **Depends on the `claude` CLI supporting `--session-id`** (verified supported on versions as of 2026-07); versions too old to recognize the flag will error out **visibly** in the pane, not fail silently.
- **Any step that fails degrades honestly.** Can't write the settings file / can't get the directory? It falls back to "inject session-id only", or even just BEL + exit signals — it never pretends observation is working.
- **Observation only reads what was actually printed / actually logged; if it can't get something, it leaves it blank** (the UI shows `—`). It never guesses, never fabricates a model name or activity state.

> Which fields deep observation actually parses out, how active / stale is judged, where the sub-agent tree comes from — see [What conmux Observes](observation.md). The onboarding source of truth in code is `conmux-app/src/observe/` (session-observer and `parsers/claude.ts`); hook construction lives in `src/lib/claude-hooks.ts`.

## So What About a "Generic Adapter Layer" — What's Missing, and Where It's Headed

To be honest about the gap:

- **What exists today**: any CLI → launched as a supervised pane (generic); Claude Code → deep observation (dedicated).
- **What doesn't exist today**: a generic scaffolding where "you toss it in and the framework type is auto-detected, with the matching observation / adapter applied automatically". The observation layer today is **one hand-written adapter per framework** — Claude has one, other frameworks don't yet.
- **To make conmux recognize your framework**, the path is the same one Claude took: write a parser / observation adapter for it. For the extension points involved — and the dividing line of "how far mechanism should reach, and what semantics should be left to whom" — see [Mechanism vs. Semantics: Where the Boundary Is](boundary.md).

If you're building your own agent CLI and want conmux to watch over it properly on Windows — **Tier 1 works right now**, so feel free to hook up directly. If you want to push the generic adapter layer forward, or get your framework added to deep observation, you're especially welcome to [open an issue / discussion, or just send a PR](https://github.com/Verson1daddy/Conmux/issues). Making "hook your agent CLI up to Windows with ease" actually true is exactly what this project most wants to build with you.