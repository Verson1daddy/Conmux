# 差异速查表

*（这一页写给已经用惯 tmux 的人。假设你知道什么是 prefix、pane、split，只想知道在 Windows 上、在 Conmux 里，哪里一样、哪里不一样。）*

## 一句话先讲清楚

Conmux 借了 tmux 的**手感**——一个前缀键（leader），然后按一下命令键去分屏、切焦点、缩放——但它**不是 tmux 的替代品（drop-in replacement）**。它不读你的 `.tmux.conf`，命令集是 tmux 的一个小子集，跑在 Windows 原生底层上。所以：肌肉记忆大体能用，但别指望把 tmux 配置搬过来就直接跑。

## Windows 原生，不用 WSL

这是 Conmux 存在的头号理由。tmux 从设计上就不为 Windows 而生——在 Windows 上用它，你得先装一层 WSL（Linux 子系统），然后你的 tmux 其实活在那个 Linux 里，管不了 Windows 这边的进程。

Conmux 直接跑在 Windows 自己的伪终端（**ConPTY**）上，**不需要 WSL**。PowerShell、原生程序、AI agent CLI，都是被同一套整树监管的 pane。（想把 WSL 里的工具也接进来？可以——但那属于路线图上的「跨 WSL 统一」（M4），**目前还没做**，另有[单独一节](wsl.md)讲当前能做到哪一步。）

## 前缀键：默认 `Ctrl+B`，为什么不是 `Ctrl+Space`

好消息：Conmux 的默认前缀就是 **`Ctrl+B`**，和 tmux 自身默认一模一样，老手上手零成本。

我们其实先试过 `Ctrl+Space`（不少人爱用的 tmux 自定义前缀）。踩了个坑：**中文 Windows 上，`Ctrl+Space` 是输入法「中/英切换」的全局热键，会被输入法先吞掉，Conmux 根本收不到**——leader 在中文环境里等于失效。所以默认改回 `Ctrl+B` 避开它。

前缀键可以改（在应用里配置，本地持久保存）。唯一的硬约束：**前缀必须带 `Ctrl` 或 `Alt`**——裸键当前缀会把每一次普通按键都吞成命令、直接弄坏你的 CLI，这条不允许。

> **和 tmux 一样的小细节**：连按两次前缀（`Ctrl+B` 再 `Ctrl+B`）会把一个字面前缀字符送进当前终端——对应 tmux 的 `send-prefix`。

## 键位对照表

按下前缀（默认 `Ctrl+B`）后，再按命令键。下面是 tmux 与 Conmux 的对照：

| 想干什么 | tmux（默认） | Conmux | 说明 |
|---------|-------------|--------|------|
| 竖切（左右并排） | `prefix %` | `prefix \` | Conmux 用 `\`，不是 `%` |
| 横切（上下堆叠） | `prefix "` | `prefix -` | Conmux 用 `-`，不是 `"` |
| 在 pane 间移动焦点 | `prefix ←↑↓→` | `prefix ←↑↓→` | 一致 |
| 调 pane 大小 | `prefix Ctrl+←↑↓→` | `prefix Shift+←↑↓→` | Conmux 用 `Shift`+方向 |
| 缩放 pane 到全屏（切换） | `prefix z` | `prefix z` | 一致 |
| 跳到第 N 个会话 | `prefix 0..9`（切窗口） | `prefix 1..9` | Conmux 跳的是「会话」，从 1 起 |
| 下一个 / 上一个会话 | `prefix n` / `prefix p` | `prefix n` / `prefix p` | 一致 |
| 打开命令面板 | `prefix :`（命令行） | `prefix :` | Conmux 开的是应用内命令面板 |

> **诚实提醒**：这张表覆盖的就是 Conmux 目前实现的 leader 命令，**没有更多了**。tmux 里的会话/窗口/pane 三级模型、复制模式（copy-mode）、`prefix [` 翻页选择、`prefix d` 快捷键 detach（Conmux 的 detach 走关闭客户端窗口，不是快捷键）、命令别名、`bind-key` 自定义绑定——这些**都还没有**。别照着 tmux cheatsheet 逐条试。

## 免前缀直接快捷键（opt-in，默认关）

嫌两步前缀麻烦？Conmux 有一档**免前缀**的直接快捷键，一步到位：

- `Ctrl+Alt+H/J/K/L` → 按 vim 方向跳焦点 pane
- `Ctrl+Alt+\` → 竖切 · `Ctrl+Alt+-` → 横切 · `Ctrl+Alt+Z` → 缩放

但它**默认是关的**，得你在设置里显式打开。为什么默认关：Conmux 的核心承诺是「**永不弄坏你的 CLI**」——默认状态下它只截走前缀键那**一个**键，其余全部原样透传给终端。开了直接快捷键，就等于多让它截走一小撮 `Ctrl+Alt` 组合；这是拿一点便利换透传的纯粹性，所以交给你自己决定要不要开。（另：真按 AltGr 组字符时不拦，非 US 键盘布局也不会被弄坏输入。）

## 明确说清楚的差异（别踩坑）

- **不读 `.tmux.conf`。** Conmux 没有任何 tmux 配置文件解析。前缀键和直接快捷键在应用内配置、存本地，和 tmux 的配置文件互不相通。
- **命令集是子集，不是全集。** 上面对照表之外的 tmux 功能大多没有；把它当「一个借了 tmux 手感的 Windows 原生复用器」，而不是「Windows 版 tmux」。
- **detach / attach 语义不同。** tmux 的会话活在服务端、`prefix d` 解绑；Conmux 是**关掉客户端窗口，pane 在后台继续跑**，下次 `attach` 画面原样重放（滚动历史 + 终端模式状态都在）。但注意：pane 活过的是**客户端**，不活过守护进程——`conmux kill-server` 或守护进程崩溃会连根带走所有 pane（这是「零孤儿进程」保证的另一面，[控制面一章](../advanced/control-plane.md)有细说）。
- **还年轻。** Conmux 是 v0.1.x。上面标了「一致」的键位是当前实现里可用的；标了「还没有」的就是真没有。手册会一处处如实标出，不把路线图当现状讲。

## 接下来

- 想把 WSL 里的工具接进来？→ [WSL 怎么办](wsl.md)
- 想从代码驱动它、做自动化？→ [进阶 · 控制面](../advanced/control-plane.md)
