# conmux

**A Windows-native terminal multiplexer: let your PowerShell, WSL and AI agents live in one supervised session.**

conmux is an independent product, not an internal component. It is the terminal foundation that [Conflux](https://github.com/Verson1daddy/Conflux) (a multi-agent CLI workbench) is built on — but it stands on its own: if you download conmux you get a terminal multiplexer, nothing else.

> **Status: early development (v0.1.x).** What exists today is the mechanism-layer Rust library that already powers Conflux in production use: real ConPTY panes, whole-tree process supervision, a single audited input path, scrollback/capture, themes, and a frozen wire-protocol type layer — backed by 60+ tests including real-process integration tests. The standalone daemon (detach/persistence), native GUI shell, and cross-WSL session unification are the active roadmap. APIs outside the protocol layer are unstable.

## Why conmux

Windows Terminal is a fine terminal emulator — and it is exactly that. Three things sit structurally outside its scope:

1. **Process-level session persistence** — close the window, your processes keep running; reattach later. (Windows Terminal's session restore is officially scoped to text-buffer snapshots; tmux upstream will never target native Windows.)
2. **A programmable control plane** — a multiplexer you can drive from code: framed request/response, per-pane event streams, stable pane IDs, capture with ANSI on/off.
3. **Unified Win/WSL process supervision** — one session tree where a PowerShell pane and a WSL pane are equally supervised, killable, and addressable. Every existing option owns only half: tmux-in-WSL can't manage Windows processes; Windows-side wrappers can't gracefully terminate WSL ones. conmux's goal is to own the boundary.

Existing "native tmux for Windows" projects deliberately route *around* WSL. conmux's main axis is to *unify* it.

## What works today (library layer)

- **Real ConPTY panes** — spawn, resize, kill, respawn; DSR (`ESC[6n`) answered inline so TUI apps don't hang.
- **Whole-tree supervision** — every pane lives in a Job Object (`KILL_ON_JOB_CLOSE`): no orphaned grandchildren, ever.
- **Single audited write path** — all input goes through one injection channel with pluggable hooks (policy / audit / rate-limit), fail-closed.
- **Scrollback & capture** — line-indexed scrollback, capture with ANSI stripping switch and effectively-full detection.
- **Event stream** — `PaneOutput` (sequenced) / `PaneExited` (exact exit codes) via a pluggable event sink.
- **Themes** — built-in registry (base24-style slots), runtime switchable, broadcast on change.
- **Protocol types** — frozen request/op/reply/payload wire types (`deny_unknown_fields`), ready for the daemon.

## Roadmap (short version)

- **M2 — daemon**: detach/attach, named-pipe IPC, VT replay. "Close the window, nothing dies."
- **M3 — native GUI shell**: tabs/panes, theme switching as a first-class control, tab tear-out & merge, collapse-to-dot.
- **M4 — cross-WSL**: WSL domains (`local` / `wsl:<distro>`), Windows-side daemon owning all ConPTYs, an in-WSL signal proxy for graceful cross-boundary termination, path translation.

## Principles

- **Mechanism, not semantics** — conmux knows panes, processes and bytes. It does not know what an "agent" is; that belongs to consumers like Conflux.
- **Local, no accounts, no telemetry.**
- **Optimized for real workloads**, not synthetic benchmarks.
- **The library is the product** — small, stable public surface (protocol + core traits); everything else explicitly unstable.

## Using the library

```toml
[dependencies]
conmux = "0.1"
```

The mechanism layer is pure-logic testable cross-platform; the ConPTY/Job Object backend compiles on Windows (Win10 1809+).

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache License 2.0](LICENSE-APACHE), at your option.
