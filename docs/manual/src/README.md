# Conmux 手册

Conmux 是一个**为 Windows 而生的类 tmux 中间件**：把任意 CLI —— 原生程序、PowerShell、乃至 WSL 里的工具 —— 接成同一套受监管的会话。它的目标很朴素：让 agent CLI 在 Windows 上**跑得起来、看得住**，又不用背上一整个 IDE。

这份手册按「你是谁」分成几条路径，挑最贴近你的那条读就行：

| 你是… | 从这里开始 |
|-------|-----------|
| **第一次用终端复用器**（没听过 tmux 也没关系） | [这是什么、为什么值得用](guide/what-and-why.md) |
| **tmux 老手**，只想知道 Windows 上哪里不一样 | [差异速查表](from-tmux/differences.md) |
| 想**从代码驱动它 / 做自动化** | [进阶 · 控制面](advanced/control-plane.md) |
| 在做**自己的 agent 框架**，想接进来 | [给 agent 框架作者](authors/onboarding.md) |

> **老实话（诚实边界）**：Conmux 还年轻（v0.1.x），只支持 Windows。内核——会话、分屏、进程整树监管、detach/attach 断线重连——已经在真实使用里跑；跨守护进程重启的持久化、远程 attach 这些**还在路上**。手册里会一处处如实标出「已经能用」和「还没做」，绝不把路线图当现状讲。

如果哪里读着别扭、装不上、或者你有更好的点子，[开个 issue 或 discussion](https://github.com/Verson1daddy/Conmux/issues) 告诉我。这是一个**华南师范大学学生**的开源项目，很想和你一起，把 Windows 上的 agent 生态做得更好。
