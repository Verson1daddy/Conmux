# Bringing In Tools from WSL

You already know Conmux runs directly on Windows' own terminal substrate (ConPTY) and **doesn't need WSL**. But plenty of people actually have WSL at hand — with the whole Linux toolchain installed inside, and some CLI that only runs smoothly in Ubuntu. This page covers: how to bring up a command inside WSL as a supervised Conmux pane.

## Already works: launch a WSL pane in one click

In the GUI's **Home add-item** flow, there's a **WSL picker**. Open it, and Conmux asks your system which distributions are installed, then lays them out in a row for you to pick from:

- Under the hood it runs `wsl.exe --list --quiet` — it just **probes** what's installed and doesn't start anything.
- Got `Ubuntu`, `Debian`, `Ubuntu-22.04`... installed? They all show up;
- **No WSL, or not a single distribution installed?** The picker just stays empty, and you fall back to the plain "text add-item" and type a command by hand — no errors, no hangs.

Pick a distribution, fill in the CLI to run (say `bash`, `htop`, or some Linux-side agent command), and Conmux assembles this command for you and brings it up as a new pane:

```
wsl -d <distro> -- <your CLI>
```

For example, pick `Ubuntu` and fill in `htop` as the CLI, and what launches is `wsl -d Ubuntu -- htop`. This pane is treated **exactly the same** as a PowerShell pane: it can split, switch, keep running in the background after you detach and close the client, and the screen picks up right where it left off on the next attach. Whole-tree supervision (Job Object) applies too — close this pane, and the processes it spawned on the outside get cleaned up with it.

> **Tip**: the picker just **fills in** the `wsl -d ... -- ...` line for you. If you'd rather type it yourself, write that whole line in the plain add-item flow and launch it as a command with arguments — the effect is exactly the same. The picker saves effort; it's not the only entry point.

## Not done yet (M4 roadmap): a unified Win/WSL "session"

Let's be clear here, so you don't overestimate what Conmux can do today.

**What works now**: bringing up a CLI inside WSL as an **independent supervised pane** — watchable, reconnectable after a disconnect. That's it — under the hood it's "Conmux launches `wsl.exe`, and `wsl.exe` itself enters WSL to run your command."

**What doesn't work yet (all on the M4 roadmap, unimplemented)**:

- **Graceful termination across the boundary (signal proxying)** — letting the Windows-side daemon cleanly signal processes inside WSL to wind down, instead of just yanking the outer `wsl.exe`;
- **Path translation** — automatic conversion between Windows paths (`D:\foo`) and WSL paths (`/mnt/d/foo`);
- **One unified Win/WSL session tree** — letting a PowerShell pane and a WSL pane be addressed, terminated, and managed as equals under **the same supervision semantics**.

These three things together are the "owning the Win/WSL boundary" goal Conmux is actually after. Today it's **not implemented yet** — it's an M4 target. So for now, use it like this: **treat a WSL CLI as a pane that's convenient to bring up**, not as deep cross-boundary unification — that's still on the way.

## Next up

- Want to see exactly how things differ from tmux on Windows? → [Differences at a Glance](differences.md)
- Want to drive these panes from code? → [Driving conmux from Code](../advanced/control-plane.md)
