# 进阶 · 控制面：从代码驱动 Conmux

*（这一页面向想做自动化、写工具、或把 Conmux 接进自己脚本/程序的人。假设你会一点 Rust；不写代码只想按键操作的话，这页可以跳过。）*

## 一句话先说清楚

大多数复用器你只能用键盘去戳它。Conmux 不一样：**它背后有一个你可以从代码里直接驱动的控制面**——开会话、发按键、抓屏幕、跟着某个 pane 的输出一路看下去，全都能程序化地做，而不只是靠快捷键。

## 骨架长这样：一个 daemon + 一堆瘦客户端

Conmux 是 tmux 那种「服务端」模型：

- **daemon（守护进程）持有所有真实的 ConPTY**。你开的每一个 pane，进程、它的整棵子进程树、滚屏历史、终端模式状态，都活在这一个 daemon 里。
- **客户端是「瘦」的**——不管是命令行 `conmux`、图形壳，还是你自己写的程序，都只是通过一条**命名管道（named pipe）**连上 daemon 跟它说话。客户端本身不持有 pane。

这就是为什么「关掉客户端窗口，里面的东西不会死」：死的只是那根连线，pane 还在 daemon 里跑。（反过来：daemon 自己没了——`kill-server` 或崩溃——所有 pane 才会一起走，这是「零孤儿进程」保证的另一面，见 [机制 vs 语义：边界在哪](../authors/boundary.md)。）

> **信任边界**：这根管道只对**当前用户**开放（DACL 只授权你自己的 SID，并拒绝远程客户端）。控制面是**本地**的——没有账号、没有遥测，不出这台机器。

## 控制面给你三样东西

回给源码核实过，`conmux` crate 对外暴露的这三件是控制面的核心：

1. **请求 / 应答**——一问一答的成帧协议。你发一个操作（`MuxOp`），daemon 回一个应答（`MuxReply::Ok { payload }` 或 `Err`），带 `correlation_id` 对得上号。
2. **每 pane 的事件流**——订阅某个 pane，就能收到它的 `PaneOutput`（带**逐 pane 严格单调、从 1 起**的 `seq` 序号）和 `PaneExited`（带确切退出码）。
3. **稳定的 pane id**——每个 pane 有一个稳定的 `PaneId`，你用它来寻址：发送、抓取、resize、attach 都认这个 id。

有了这三样，你就能**从代码驱动它，而不是只靠按键**。

## 从 Rust 里驱动它

客户端入口在 `conmux::client::Client`（下面的方法名都回 `client.rs` 核实过）：

```rust
use conmux::client::Client;
use conmux::protocol::MuxOp;

// 连上当前用户的 daemon；没有 daemon 就自动帮你拉起一个（tmux 那种心智）。
let mut client = Client::connect_or_spawn()?;

// 一问一答：列出当前所有 pane。
let panes = client.request(MuxOp::ListPanes)?;

// 往某个 pane 注入按键（原始字节，走 daemon 内唯一那条受审计的写链）。
client.request(MuxOp::Send { pane_id: id.clone(), data: b"ls\r".to_vec() })?;

// 抓一个 pane 当前的屏幕（可选带/不带 ANSI）。
let snap = client.request(MuxOp::Capture(cap_req))?;
```

想**跟着一个 pane 一路看下去**（而不只是抓一张快照），用 `attach`——它先给你一份原子快照（终端模式前导 + 滚屏历史 + 序号高水位），再转成一个流式会话，之后循环收 live 输出、也能回注 stdin：

```rust
let attached = client.attach(&id)?;
// 先按 mode_preamble → history → buffered 的顺序喂给渲染器重建画面
let mut session = attached.session;
while let Some(ev) = session.recv_output() {
    // AttachEvent::Output { seq, data } / Exited { exit_code }
}
```

其它常用操作（都是 `MuxOp` 的变体，回 `protocol.rs` 核实）：`Spawn` / `Respawn`（原子同 id 重起）/ `Resize` / `KillTree` / `Subscribe` · `Unsubscribe` / `ListThemes` · `SetTheme` / `KillServer`。完整清单和每个 payload 的形状见 [下一页 · 协议](protocol.md)。

## 诚实边界：哪些稳、哪些会变

Conmux 的稳定性承诺**分成两档**，这点很重要——回 `crate::lib` 的文档核实：

- **✅ 协议层是冻结契约（committed）**。wire 协议类型（`MuxRequest` / `MuxOp` / `MuxPayload` / `MuxReply` / `MuxNotify`，及其携带类型闭包，如 `PaneId` / `PaneSize` / `PaneState`）、`PaneHost` 门面、事件面、注入扩展点、主题面——这些在 0.x 期间变更**必须走 minor 版本号 + CHANGELOG**，patch 版本不会破坏你。序号语义（`PaneOutput.seq` 逐 pane 从 1 起严格单调）也随附冻结。**按协议层建自动化是可以放心的。**
- **⚠️ 协议层以外的 API 是 unstable，1.0 前可能不打招呼就变**。具体说：`daemon`、`client`、`pipe`、`wire` 这几个模块在源码里都明确标了「Stability: unstable — may change without notice」。也就是说，上面那段 `Client::connect_or_spawn()` / `attach()` 的**具体 Rust 方法形态**属于会变的一档——能用、Conflux 自己也在真实用它，但别把它当冻结契约。想要长期稳的锚，锚在**协议本身**（wire 类型），而不是某个客户端方法签名。

> 一句话拿捏：**协议是合同，客户端 API 是当前实现**。做自动化时，越贴近 wire 协议，越不容易被未来版本掀翻。

## 接下来

- 想看完整的操作清单、每个请求/应答的字段、事件帧长什么样？→ [进阶 · 协议](protocol.md)
- 想把自己的 agent 框架接进来？→ [给 agent 框架作者](../authors/onboarding.md)
