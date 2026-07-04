# 接 WSL 里的工具

你已经知道 Conmux 直接跑在 Windows 自己的终端底层（ConPTY）上，**不需要 WSL**。但很多人手边真有 WSL——里面装着 Linux 那套工具链，某个 CLI 只在 Ubuntu 里跑得顺。这一节讲：怎么把 WSL 里的一个命令，当成 Conmux 的一块受监管 pane 起起来。

## 已经能用：一键起一个 WSL pane

在 GUI 的 **Home 加项** 里，有一个 **WSL picker**。打开它，Conmux 会去问一句你系统里装了哪些发行版，把它们列成一排给你选：

- 后台跑的是 `wsl.exe --list --quiet`，只是**探一下**装了什么，不会启动任何东西。
- 装了 `Ubuntu`、`Debian`、`Ubuntu-22.04`… 就都列出来；
- **没装 WSL、或者一个发行版都没有**？picker 就空着，你直接退回普通的「纯文本加项」手动敲命令即可——不会报错、不会卡住。

选好发行版、填上要跑的 CLI（比如 `bash`、`htop`、或某个 Linux 侧的 agent 命令），Conmux 就替你拼出这么一条命令，把它当一块新 pane 起起来：

```
wsl -d <发行版> -- <你的 CLI>
```

比如选了 `Ubuntu`、CLI 填 `htop`，起的就是 `wsl -d Ubuntu -- htop`。这块 pane 和一块 PowerShell pane **一视同仁**：同样能分屏、切换、detach 关掉客户端后在后台继续跑、下次 attach 画面原样接回来。整树监管（Job Object）也照管——关掉这块 pane，它在外层拉起的进程一起收走。

> **小提示**：picker 只是帮你把 `wsl -d ... -- ...` 这行命令**填好**。你要是喜欢自己敲，在普通加项里手写这一整行、当成一条带参数的命令起，效果完全一样。picker 是省事，不是唯一入口。

## 还没做（M4 路线图）：跨 Win/WSL 的「统一会话」

这里要把话说清楚，别让你误会 Conmux 现在能做到多少。

**现在能做的**：把 WSL 里的一个 CLI，当一块**独立的受监管 pane** 起起来、看得住、能断线重连。就这么多——它本质上是「Conmux 起了 `wsl.exe`，`wsl.exe` 自己再进 WSL 跑你的命令」。

**现在还做不到（都在 M4 路线图上，未实现）**：

- **跨边界的优雅终止（信号代理）**——让 Windows 侧的守护进程能干净地给 WSL 里的进程发信号收尾，而不只是硬拔掉外层的 `wsl.exe`；
- **路径转换**——Windows 路径（`D:\foo`）和 WSL 路径（`/mnt/d/foo`）之间自动翻译；
- **一棵统一的 Win/WSL 会话树**——让一块 PowerShell pane 和一块 WSL pane 在**同一套监管语义**下被同等地寻址、终止、管理。

这三件事合起来，才是 Conmux 真正想做的那件「拥有 Win 与 WSL 的边界」的事。今天它**还没实现**，是 M4 的目标。所以现阶段请这样用它：**把 WSL CLI 当一块方便起起来的 pane**，而不是指望它做深度的跨边界统一——那还在路上。

## 接下来

- 想看 Windows 上和 tmux 到底哪里不一样？→ [差异速查表](differences.md)
- 想从代码里驱动这些 pane？→ [进阶 · 控制面](../advanced/control-plane.md)
