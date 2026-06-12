//! Pane 事件出口（API 契约 §9 / §4.4）。
//!
//! conmux **不依赖 Tauri**——不调 `tauri::Emitter::emit`，而是把 per-pane 事件经
//! [`PaneEventSink`] 回调交给消费方（conflux 在 sink 实现里转 Tauri emit / AttentionQueue
//! ingest）。这结构性消除现状 `core/event_emit.rs` 反向依赖 orchestration 的"core 不纯"。
//!
//! **与契约 §7 字面 MuxNotify 的偏离（机制/策略分层）**：契约草案的 MuxNotify 含
//! `data_base64: String` 与 `PaneStateChanged{status: AgentStatus}`。conmux 实测落地时
//! 修正为：(1) `PaneOutput.data` 用**原始 `Vec<u8>`**——base64 是 conflux IPC 边界的编码
//! 关注点，不属机制层；(2) **去掉 PaneStateChanged**——`AgentStatus`（思考/等权限）是
//! conflux 对 PTY 内容的语义解读，conmux 只认 `PaneLifecycle`（裁决②）。语义状态由 conflux
//! 在 sink 实现里据 PaneOutput/hook 推断。conmux 只发它机制层确知的事件。

use crate::types::PaneId;

/// conmux 向消费方推送的 per-pane 异步事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxNotify {
    /// pane 原始输出（`seq` 为 per-pane 单调序号，供重放对账；data 为原始字节）。
    PaneOutput {
        pane_id: PaneId,
        seq: u64,
        data: Vec<u8>,
    },
    /// pane 进程退出（exit_code 不可得时 None，不静默伪装，D9）。
    PaneExited {
        pane_id: PaneId,
        exit_code: Option<i32>,
    },
}

/// 事件出口 trait。conmux 把 per-pane 事件推给消费方；conflux 实现它（内部转
/// Tauri emit / AttentionQueue ingest）。
///
/// **节流 = 无损合帧（复闸 C6）**：若消费方/conmux 对 PaneOutput 做合帧，只能拼接
/// 不得丢字节、`seq` 连续——丢帧会让消费方据残缺输出做决策（据残缺批权限）。
pub trait PaneEventSink: Send + Sync {
    fn on_notify(&self, notify: MuxNotify);
}
