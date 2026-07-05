# FAQ

Here are the questions people ask most often, in Q&A form, in plain language. Anywhere something is "not done yet," we say so — no hedging.

## Do I need WSL first?

**No.** Conmux runs directly on Windows' own terminal foundation (**ConPTY**), and every pane is a real Windows pseudoterminal. Install it and you can run PowerShell, `cmd`, and any native command-line program — no Linux subsystem layered underneath.

What about tools that live in WSL? **You can bring them in** — run `wsl.exe <your tool>` directly in a pane, and Conmux takes it over, supervises it, and reconnects it after a disconnect just like any ordinary command. This path works today.

> **Honest note**: Supervising Windows-side and WSL-side processes **unified in a single session tree** (e.g., gracefully shutting down WSL processes across the boundary, automatic path translation) — that layer is not built yet; it's **M4** on the roadmap. Today you can run WSL tools in a pane, but that "everything treated equally under one unified supervisor" story is still on the way.

## How does it relate to tmux?

Conmux is a **tmux-like** thing — a terminal multiplexer (multiple sessions in one window, split panes, reconnect after disconnect). But it is **not a port of tmux, and not a drop-in replacement**:

- **tmux was not built for Windows**, and upstream has explicitly said it won't do native Windows; to use tmux on Windows, you have to crawl into WSL first.
- **Conmux is Windows-native from the ground up** — it runs on ConPTY, with no Unix emulation layer.

So tmux config files, `.tmux.conf`, and that whole set of shortcuts **don't carry over**. Conmux has its own (in the GUI the default prefix is `Ctrl+B` — deliberately matched to tmux for the sake of veterans, but the implementations underneath are two different things). If you're a tmux veteran, the fastest path is [Differences at a Glance](../from-tmux/differences.md).

## Is it a memory hog?

**Depends on which layer you use — different answers, and here's the honest version:**

- **Conmux with the graphical interface (conmux-app)**: it uses Tauri plus the system's built-in **WebView2**. This is **not** the most extreme lightweight option; the memory floor is roughly **a few hundred MB**. It's far leaner than "stuffing your agents into a full IDE (VS Code plus a pile of extensions)," but if "extreme memory frugality" is the bar, it isn't that.
- **The pure command-line `conmux` (the GUI-less crate / CLI)**: **very light**. It's a pure-Rust mechanism-layer kernel with a deliberately tiny dependency tree — no editor, no browser-engine blob.

In one sentence: chasing lightweight and living entirely in the command line → use the pure CLI `conmux`; want visual split panes, session dots, and deep observation panels → use the GUI shell and accept the few-hundred-MB floor.

## Does it support macOS / Linux?

**No, and that's by design, not for lack of time.** Conmux is **Windows only**: its entire reason to exist is "bring tmux-class capabilities to native Windows" — ConPTY pseudoterminals, Job Object whole-tree supervision, named-pipe IPC — all of it Windows platform machinery. On other systems, you already have tmux / Zellij.

Technically: the GUI shell compiles and runs only on Windows (Win10 1809+ / Win11). The `conmux` crate's **pure logic layer** compiles and tests cross-platform (for development convenience), but the ConPTY / Job Object backends that do the real work compile only on Windows.

## How does it relate to Conflux?

In short: **Conmux is the foundation, Conflux is the building on top of it.**

- **Conmux** understands exactly three things — panes, processes, and bytes. It **has no idea what an "agent" is**, and doesn't want to. That boundary — "mechanism only, never semantics" — is precisely what qualifies it to be a foundation. It's a standalone product: download Conmux on its own and what you get is a terminal multiplexer, nothing more.
- **Conflux** is built on top of Conmux — a **multi-agent CLI supervision console (GUI)** that adds the "agent" semantics back in: who's waiting on you, who popped a permission request, and how to arrange multiple agents.

So: want a Windows-native terminal-multiplexing foundation → Conmux; want visual multi-agent supervision on top of it → Conflux.

## It's unsigned — is it safe to install?

First, the current state of things: the GUI installer will be **unsigned** — this is a student open-source project with no budget for a code-signing certificate. Windows SmartScreen will warn "Unknown publisher." You can click "More info → Run anyway," or just build from source yourself. (The pure CLI `conmux` installs via `cargo install --locked conmux`, which never triggers that warning at all.)

**Why you can be at ease:**

- **The source is fully open** — dual-licensed MIT / Apache-2.0; you can read all of it, audit it, and `cargo build` / `npm run tauri:build` your own hand-compiled copy.
- **No local telemetry** — no accounts, no analytics reporting, **no data ever leaves your machine**. Connection-level auditing (who connected to the daemon) is written only to one local rolling log, `%LOCALAPPDATA%\conmux\daemon.log`, and never sent anywhere.
- **The trust boundary is "the current user"** — the daemon runs over named pipes, with a DACL that admits only your own account (SID) and rejects remote clients.

> **Honest note**: The identity checks at the named-pipe layer (client fail-closed; client verifying the daemon's process image via Authenticode) exist to "raise the bar and stay auditable" — they are **not** there to defend against malicious code already running under your own account. Any program running as you can already read your memory and kill your processes. That's the operating system's boundary, not something Conmux can patch over for you. We lay this out plainly rather than pretend otherwise.

---

Still have an unanswered question? Feel free to [open an issue or discussion](https://github.com/Verson1daddy/Conmux/issues) and help us make this FAQ more complete.