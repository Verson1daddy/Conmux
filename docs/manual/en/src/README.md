<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./logo-dark.svg">
    <img alt="conmux" src="./logo-light.svg" width="84">
  </picture>
</p>

# The Conmux Manual

Conmux is a **tmux-like middleware built for Windows**: it hooks up any CLI — native programs, PowerShell, even tools inside WSL — into one set of supervised sessions. Its goal is modest: let agent CLIs on Windows **run and stay watchable**, without dragging in a whole IDE.

This manual is split into paths by "who you are" — just pick the one closest to you:

| You are… | Start here |
|-------|-----------|
| **New to terminal multiplexers** (never heard of tmux? that's fine) | [What It Is & Why It's Worth Using](guide/what-and-why.md) |
| A **tmux veteran** who just wants to know what's different on Windows | [Differences at a Glance](from-tmux/differences.md) |
| Looking to **drive it from code / automate things** | [Advanced · Driving conmux from Code](advanced/control-plane.md) |
| Building **your own agent framework** and want to hook it up | [Hooking Your CLI Up to conmux](authors/onboarding.md) |

> **Honest note (where the honest boundary is)**: Conmux is still young (v0.1.x) and Windows-only. The core — sessions, split panes, whole-process-tree supervision, detach/attach reconnection — is already running in real use; persistence across daemon restarts and remote attach are **still on the way**. Throughout this manual, "works today" and "not built yet" are labeled honestly, point by point — the roadmap is never presented as the present.

If anything reads awkwardly, won't install, or you have a better idea, [open an issue or discussion](https://github.com/Verson1daddy/Conmux/issues) and tell me. This is an open-source project by a **South China Normal University student**, and I'd love to work with you to make the agent ecosystem on Windows better.