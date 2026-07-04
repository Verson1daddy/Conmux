# Wire 协议参考

*（这一页是给要**从代码接进 daemon**、或者想搞明白管道上到底跑什么字节的人看的。只用图形界面或 CLI，可以跳过。）*

Conmux 的 daemon 和它的客户端（CLI、GUI、你自己写的程序）之间，在一条**命名管道**上收发一串 JSON 帧。这一页把这串帧的形状写清楚——每一条都对应 `crates/conmux/src/protocol.rs` 里 `#[derive(Serialize, Deserialize)]` 的真实类型，不是另起炉灶的文档。

> **稳定性提醒**：协议层是 conmux **唯一承诺稳定**的公开面（serde 形状即契约，破坏性改动走 minor + CHANGELOG）。协议层之外的 API 都还不稳定。下面每个类型名都能在 `protocol.rs` / `event.rs` 里一一对上。

## 一条连接上跑什么：帧信封 `WireFrame`

所有帧都裹在一个信封枚举 `WireFrame` 里，externally tagged（JSON 里是 `{"变体名": {...}}`）。一共五种：

| 帧 | 方向 | 载荷 |
|----|------|------|
| `Hello` | 客户端 → daemon | `protocol_version: u32`、`client_kind: String` |
| `HelloAck` | daemon → 客户端 | `protocol_version: u32`、`daemon_version: String` |
| `Request` | 客户端 → daemon | 一个 `MuxRequest` |
| `Reply` | daemon → 客户端 | 一个 `MuxReply` |
| `Notify` | daemon → 客户端 | 一个 `MuxNotify`（异步事件，无相关号） |

**方向是有约束的，不是随便发**：daemon 只接受 `Hello`（且仅在握手期）和 `Request`；客户端只接受 `HelloAck` / `Reply` / `Notify`。发反了 = 协议错误，直接断连。

**握手先行**：客户端连上后**首帧必须是 `Hello`**。`client_kind` 只是个自由标签，进审计日志，**不参与任何授权判定**——别指望靠它提权。

## 版本必须严格相等

`PROTOCOL_VERSION` 当前是 `1`，它**独立于 crate 版本**。握手时 daemon 拿自己的版本和 `Hello` 里的 `protocol_version` 做**严格相等**校验——不是"大于等于"，是"必须一样"。对不上就不给你 `HelloAck`。任何 wire 形状的破坏性变更都要 bump 这个常量。

## 拒收未知字段（`deny_unknown_fields`）

`WireFrame` 信封、`Hello` / `HelloAck`、以及 `MuxOp` 都开了 `deny_unknown_fields`：报文里多一个没定义的键，**反序列化直接失败**，而不是静默忽略。这条最要紧的用途是把注入源挡在门外——见下面 `Send`。

## 请求：`MuxRequest` 与操作枚举 `MuxOp`

一个请求 = 相关号 + 一个操作：

```
MuxRequest { correlation_id: u64, op: MuxOp }
```

`correlation_id` 用来把应答和请求配对（应答会带回同一个号）。`MuxOp` 是全部能下达的操作，逐一列在下面。右列是这个操作成功时对应的 `MuxReply::Ok` 载荷（`MuxPayload` 的变体）。

| `MuxOp` 变体 | 字段 | 成功载荷（`MuxPayload`） | 备注 |
|-------------|------|------------------------|------|
| `Spawn` | `SpawnRequest` | `Spawned(PaneId)` | 起一个新 pane |
| `Respawn` | `SpawnRequest` | `Spawned(PaneId)` | 原子同 ID 重起（消掉 KillTree+Spawn 之间的 ID 复用窗口） |
| `Send` | `pane_id`、`data`（见下） | `Sent` | 向 pane 注入输入 |
| `Capture` | `CaptureRequest` | `Captured(CaptureResult)` | 抓 scrollback |
| `Resize` | `pane_id`、`size: PaneSize` | `Resized` | 改 pane 行列 |
| `KillTree` | `pane_id` | `Killed` | 杀掉整棵进程树 |
| `ListPanes` | 无 | `Panes(Vec<PaneState>)` | 列出所有 pane |
| `Subscribe` | `pane_id` | `Subscribed` | 订阅该 pane 的事件流 |
| `Unsubscribe` | `pane_id` | `Unsubscribed` | 取消订阅 |
| `Attach` | `pane_id` | `AttachSnapshot { ... }` | 原子「订阅 + 快照」 |
| `ListThemes` | 无 | `Themes(Vec<TerminalTheme>)` | 列出主题预置 |
| `SetTheme` | `id: String` | `ThemeSet` | 热切换主题；另广播 `ThemeChanged` |
| `KillServer` | 无 | `ServerKillScheduled` | 终结 daemon 及全部会话 |
| `PinExecutable` | `path: String` | `Pinned` | pin 可执行文件到信任库 |
| `UnpinExecutable` | `path: String` | `Unpinned` | 移除 pin |

这就是**全部** 15 个变体，一个不多一个不少。`MuxOp` **故意不加** `#[non_exhaustive]`：新增变体是一次显式的 minor 决策，daemon 的 dispatcher 对它穷尽 match，未来加变体会在 daemon 侧编译报错，逼你显式处理，而不是静默漏掉。

### 关于 `Send`：注入源不过 wire

`Send` 的 `data` 是**原始字节**，上 wire 时经 base64 编码（方向键、Alt 组合、二进制粘贴这些非 UTF-8 内容用普通字符串无法无损携带）。

**注意 `Send` 上没有 `source` 字段**，这是故意的：注入来源由接收端边界按信道身份赋值，**不允许客户端在 wire 上自报**。配合 `deny_unknown_fields`，一条带 `source` 键的 `Send` 报文会**反序列化即失败**——拒收，而不是接受后再丢弃。

### 关于订阅类操作

`Subscribe` / `Unsubscribe` 维护「这条连接关心哪些 pane」的集合，daemon 的 fan-out（`FanoutSink`）据此把 `PaneOutput` / `PaneExited` 事件**只投给订阅了的连接**。`Attach` 是一步到位的「订阅 + 原子快照」：先注册订阅、再取当前 scrollback 快照，之后按 `seq > last_seq` 连续喂 live 流——中间不丢帧不重帧（限速 + per-pane 并发=1 防快照放大）。这套「fan-out 分发 + 无缝快照拼接」在当前 daemon 里**已经实现并有测试**（M2 里程碑已落地，见 `daemon.rs::attach_with_limits` / `FanoutSink`）。（`SetTheme` 明确**不持久化**主题偏好——持久化归上层消费者 / GUI 壳，不归 daemon。）

## 应答：`MuxReply` 与 `MuxPayload`

应答两条路，都带 `correlation_id` 供配对：

```
MuxReply::Ok  { correlation_id: u64, payload: MuxPayload }
MuxReply::Err { correlation_id: u64, error:   ConmuxError }
```

出错走 `Err`，携带的是 `ConmuxError`（**机制层**错误，比如 `PaneNotFound`）——conmux 只报它机制层的错，不认 conflux 的语义错误。

成功载荷 `MuxPayload` 的变体已在上表右列逐一对应。多数是无字段的确认（`Sent` / `Resized` / `Killed` / `Subscribed` / …）。两个带真数据的值得单独看：

- **`AttachSnapshot`**——`Attach` 的原子快照，字段：`mode_preamble_b64`（终端模式前导，如 alt-screen）、`history_b64`（scrollback 历史）、`last_seq`（末序号）、`pane_state`。客户端重建 = 喂 preamble → 喂 history → 之后按 `seq > last_seq` 连续喂 live 流，不丢不重。
- **`Captured(CaptureResult)`**——`Capture` 的结果，含 base64 数据、首/末绝对行号、是否截断、是否"实际已满"。

`MuxPayload` **加了** `#[non_exhaustive]`（与 `MuxOp` 相反），消费者 match 时要留 `_ =>` 分支。

## 异步事件：`MuxNotify`

daemon 主动往客户端推的事件，裹在 `WireFrame::Notify` 里，**没有相关号**（它不回应任何请求）。定义在 `event.rs`，三种：

| `MuxNotify` 变体 | 字段 | 含义 |
|-----------------|------|------|
| `PaneOutput` | `pane_id`、`seq: u64`、`data`（base64） | pane 的原始输出；`seq` 是 per-pane 单调序号，供重放对账 |
| `PaneExited` | `pane_id`、`exit_code: Option<i32>` | 进程退出；拿不到退出码时是 `None`，**不伪装**成 0 |
| `ThemeChanged` | `id: String` | 主题被 `SetTheme` 切换后广播，供实时换肤 |

同 `Send`，`PaneOutput.data` 上 wire 也走 base64（原始字节可能含不可打印 / 非 UTF-8 内容）。in-proc 直连的消费者（sink 实现）仍收原始 `Vec<u8>`，base64 只在管道边界生效。

`seq` 的单调性有个硬纪律：如果消费方或 conmux 对 `PaneOutput` 做合帧，**只能拼接、不得丢字节、`seq` 必须连续**——丢帧会让消费方据残缺输出做错误决策。

> **诚实边界**：`MuxNotify` 只发 conmux **机制层确知**的事件——字节、退出、换肤。它**不发**"这个 agent 在思考 / 在等权限"这类语义状态：那是上层（比如 Conflux）对 PTY 内容的解读，不属于协议层。别指望在 wire 上直接读到 agent 状态。

## 一眼核对

想快速验证上面这些，`protocol.rs` 底部的 `#[cfg(test)]` 模块就是活文档：`all_ops_round_trip` 列全了 15 个 op，`wire_frame_all_directions_round_trip` 跑五种信封帧，`send_with_source_field_is_rejected_on_wire` 和 `hello_rejects_unknown_fields` 演示了拒收未知字段的两处。改协议前，先让这些测试红，是最稳的对账方式。
