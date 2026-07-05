# What It Is & Why It's Worth Using

*(This page assumes you've never used tmux or any "terminal multiplexer". If you have, skip straight to [Differences at a Glance](../from-tmux/differences.md).)*

## The problem first

You're running things on the command line — say, an AI coding agent (Claude Code, Codex…), or a build, or a local server. One terminal window per thing. Want to run three or four at once? Then it's three or four windows, and you're clicking back and forth between them: checking one by one who's stuck, who's waiting for your input, who's finished.

Once the windows pile up, you become the "human scheduler" — constantly switching around, terrified of missing the one that's waiting on you.

## What Conmux does

Conmux puts multiple command-line sessions **inside one window**, each in its own patch (called a "pane"). In that window you can:

- **Split panes** — carve one window into a grid and watch several sessions side by side.
- **Switch** — a row of session dots represents each session; click one to jump over.
- **Leave and come back** — close the Conmux window and the sessions inside **don't die**: they keep running in the background; open it next time and the screen picks up right where it was (this is called detach / attach).
- **Keep watch** — whichever session needs you (say, an agent pops a permission request), Conmux flags it for you, instead of making you flip through them one by one.

In one sentence: **it watches those command lines for you, so you only step in when you're actually needed.**

## Why Conmux, not tmux

If you've ever searched for "terminal multiplexer", you've most likely seen **tmux** — the classic tool on Linux / macOS. The problem: **tmux was not built for Windows**. To use it on Windows, you first have to install a layer of WSL (a Linux subsystem running inside Windows).

Conmux runs directly on Windows' own terminal substrate (called **ConPTY**), **no WSL required**. And if you *do* want to bring in tools from WSL, it can do that too — there's [a dedicated section](../from-tmux/wsl.md) on it.

It's also lighter than "stuffing your agent into a full IDE (say, VS Code plus a pile of extensions)": Conmux revolves around the command line itself, without the whole editor apparatus.

> **Honest note**: the GUI version of Conmux uses the system's built-in WebView2, which is **not** the most extreme lightweight option (the memory floor is a few hundred MB). It's far leaner than a full IDE, but if you're chasing "absolute minimum memory", the pure command-line `conmux` tool (no GUI) is the lightest tier.

## Next up

- Haven't installed it yet? → [Install It](install.md)
- Installed and ready to try? → [Your First Session & Split Panes](first-session.md)
