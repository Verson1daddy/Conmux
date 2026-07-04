# conmux 观测什么

如果你在做自己的 agent 框架，最关心的多半是这句：**接进 conmux 之后，它能替我看住多少？** 这一页老实讲清楚——哪些信号对**任何** CLI 都有，哪些只有 Claude Code 能拿到，以及一条不变的底线：conmux 只观测程序**真打印、真落盘**的东西，拿不到就显「—」，绝不为了好看编一个数出来。

先说一个大前提。conmux 的观测跑在**终端窗口的图形壳**（conmux-app）里，和终端渲染并行挂一个「观测者」订阅同一份 PTY 输出——它不改你的程序、不注入代码、不猜你在干嘛，只是**读**。

## 对任何 CLI 都有：PTY 级信号

不管你接的是 PowerShell、一个编译脚本、还是别家的 agent CLI，只要它是个正常的命令行进程，conmux 就能给你这几样：

- **活跃度（running / idle）**——最近一小段时间有输出就是 `running`，静默超过约 2.5 秒翻成 `idle`。这不是"它在思考"这种判断，就是字面的"最近有没有在往终端吐字"。
- **进程退出 + 退出码**——进程结束时 conmux 拿到确切的退出码（内核层 `PaneExited` 事件带的是真实 exit code，不是估的）。窗口里那个会话的状态随之变成 `exited`。
- **终端响铃 → 需注意（attention）**——程序往终端打了一个响铃字符（BEL，`\x07`）、或者进程退出了，conmux 就把这个会话标成"需要你看一眼"。这两个都是**真信号**（响铃是 tmux `monitor-bell` 那套语义），不是启发式瞎猜。你切到那个会话看了，标记就清掉——看过了就不再打扰你。

就这些。对非 Claude 的 CLI，conmux **只提供**这一层。它不会去解析你程序的模型名、token 数、内部状态——因为它没法诚实地知道，所以干脆不编。观测状态里那些字段（model / tokens / 活动）对普通 CLI 一律是 `null`，界面上显「—」。

## 只有 Claude Code 有：深度观测

Claude Code 除了往终端打字，还会把整场会话**结构化地落盘**成一份 JSONL 文件，另外可以配一个通知 hook。conmux 认得这两样，于是能对 Claude 会话看得深得多。**这是 Claude 专属的**——靠的是 Claude Code 自己写出来的真数据，别的 CLI 没有这些落盘物，也就没有这一层。

conmux 怎么认出这是个 Claude 会话？要么你在启动时就告诉它（启动命令里带 `claude`），要么它从终端输出里嗅到 Claude 的稳定标志（比如终端标题恒为 `✳ Claude Code`、或那行 `Using Opus 4.8 (1M context)`）。认定之后，才会**懒启**下面这些源——非 Claude 会话永远不碰它们。

**读 JSONL，拿到这些：**

- **模型名（model）**——直接取会话记录里的 `message.model`（如 `claude-opus-4-8`），是字面真值，不是从终端刮的。
- **上下文占用（contextPct）**——从最后一条真实 assistant 消息的 `usage` 算：`input + cache_read + cache_creation` 除以该模型的上下文窗口（Opus / Sonnet 按 1M，Haiku 按 200K；模型不认识就不猜，显「—」）。
- **会话累计 token（Σ 输入 / 输出）**——把整场真实消息的 usage 累加起来。
- **subagent 树**——Claude 派子 agent 时会在记录里留 `tool_use`（`Agent` / 旧版 `Task`），带 `subagent_type` 和描述；对应的 `tool_result` 回来就标记这个子 agent 已完成。conmux 把它渲染成**一层扁平**的子 agent 列表（Claude 主 agent → 子 agent 就一层，不臆造更深的嵌套）。
- **正在跑的 workflow / 最近调用的 skill**——从 `tool_use` 里读出来，如实标出。

> **诚实边界（几个都写在源码注释里的坑）**：① 只有当会话里出现过**真实**的 assistant 消息（有真 usage、不是中断产生的 `<synthetic>` 空消息）才写这些字段，否则保持「—」，不拿假 0 糊弄。② 这些字段来自 JSONL——多数 Claude 版本**不**把 token 计数打到终端上，所以别指望从终端文本刮到它们；深度数据的来源是那份落盘文件。③ 要读 JSONL 得先知道会话的工作目录（cwd）；cwd 还没拿到时，深度观测会**明确**告诉界面"暂时读不到"，而不是静默地一直显「—」让你一头雾水。

**Notification hook，驱动更准的 attention：**

Claude Code 的 Notification hook 能在两种时刻发信号——**弹出权限请求框**（`permission_prompt`）和**空闲时向你提问**（`idle_prompt`）。conmux 把这类事件收下来，命中在册类型就把会话标成"需注意"。这比单纯的终端响铃更贴——它精确对上了"Claude 正卡在等你批准 / 等你回答"这个真实时刻。

关于 hook 还有一个诚实标注：conmux 只会在**真收到过** hook 事件、确证这条链路通了之后，才亮出"深度感知已生效"。它**不**反过来断言"没亮就是 hook 没工作"——也许只是这会话还没触发过权限框或空闲提问而已。只做正向标注，不做否定推断。

## 一句话收口

- **任何 CLI**：活跃度、退出码、响铃 / 退出触发的 attention。够你把一排会话"看住"。
- **Claude Code**：在上面再叠一层结构化深观测——模型、上下文、token、subagent 树，加上 hook 驱动的精准 attention。
- **底线不变**：观测 = 读它**真打印、真落盘**的东西。读不到就诚实显「—」，从不臆测、从不填假数。

要把**你自己的** agent CLI 接进来、让它至少吃到 PTY 级观测，甚至留出让 conmux 认出它的钩子，见 [把你的 CLI 接上 conmux](onboarding.md)。
