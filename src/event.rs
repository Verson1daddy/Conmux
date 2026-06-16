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

use serde::{Deserialize, Serialize};

use crate::types::PaneId;

/// conmux 向消费方推送的 per-pane 异步事件。
///
/// **serde（M2a / D-4）**：经 `WireFrame::Notify` 上命名管道。`PaneOutput.data` 用
/// base64 适配（`serde_b64`）——原始字节含不可打印/非 UTF-8 内容，JSON 字符串需编码；
/// 与 conflux 前端 IPC 的消费编码一致（DR-2 口径统一）。in-proc 消费者（sink 实现）
/// 仍收原始 `Vec<u8>`，serde 仅在 wire 边界生效。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive] // M1 契约 §1.3-④
pub enum MuxNotify {
    /// pane 原始输出（`seq` 为 per-pane 单调序号，供重放对账；data 为原始字节）。
    PaneOutput {
        pane_id: PaneId,
        seq: u64,
        #[serde(with = "serde_b64")]
        data: Vec<u8>,
    },
    /// pane 进程退出（exit_code 不可得时 None，不静默伪装，D9）。
    PaneExited {
        pane_id: PaneId,
        exit_code: Option<i32>,
    },
    /// 主题热切换广播（G-2 / D-8）：daemon 收 `MuxOp::SetTheme` 后向订阅者广播。
    /// daemon 不持久化偏好——消费者据此实时换肤。
    ThemeChanged { id: String },
}

/// `Vec<u8>` ↔ base64 字符串 serde 适配（D-4，wire 边界用）。
/// `pub(crate)`：`MuxOp::Send.data` 复用同一编码（M2a-M2，与 `PaneOutput.data` 口径统一）。
pub(crate) mod serde_b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        let enc = base64::engine::general_purpose::STANDARD.encode(bytes);
        s.serialize_str(&enc)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

/// 事件出口 trait。conmux 把 per-pane 事件推给消费方；conflux 实现它（内部转
/// Tauri emit / AttentionQueue ingest）。
///
/// **节流 = 无损合帧（复闸 C6）**：若消费方/conmux 对 PaneOutput 做合帧，只能拼接
/// 不得丢字节、`seq` 连续——丢帧会让消费方据残缺输出做决策（据残缺批权限）。
pub trait PaneEventSink: Send + Sync {
    fn on_notify(&self, notify: MuxNotify);
}
