//! Mux 协议类型冻结（API 契约 §7 / 总契约 §4.4 / MF-2）——V1-core 批次 1。
//!
//! **V1 = 类型冻结 + serde 往返，不做 handler/dispatcher**：in-proc 直调即现状
//! （conflux 命令层直走 `PaneHost` 方法）；本模块类型为 V2 命名管道传输预留——
//! 全部 `Serialize + Deserialize`，届时原样过 wire（control-mode 框架化模板）。
//!
//! ## 冻结不变量
//! - **MF-2：`MuxOp::Send` 无 `source` 字段**——`InjectionSource` 由接收端边界
//!   （`PaneHost::inject_stdin`）按信道身份赋值；本枚举启用 `deny_unknown_fields`，
//!   wire 上携带 `source` 键的 `Send` 报文**反序列化即失败**（拒收强于静默丢弃）。
//! - `MuxOp::Send` 的执行语义 = **必经** `PaneHost::inject_stdin`（注入钩子链），
//!   无其它实现路径（MF-1）；V2 daemon 的 dispatcher 实现受此约束。
//! - `MuxReply::Err` 携带 [`ConmuxError`]（机制层错误）——对总契约 §4.4 字面
//!   `ConfluxError` 的机制/策略分层修正（conmux 不依赖 conflux）；conflux 在 IPC
//!   边界经 `From<ConmuxError>` 转前端友好错误。
//! - 异步事件 [`MuxNotify`]（无 correlation）复用 `event.rs` 类型——其对总契约 §7
//!   字面的偏离（`data` 用原始 `Vec<u8>`、无 PaneStateChanged）见 event.rs 文档。
//! - `PaneOutput.seq` 单调（V2 重放对账），V1 起即填——由 PaneHost 读线程保证，
//!   e2e 断言见 pane.rs。

use serde::{Deserialize, Serialize};

use crate::capture::{CaptureRequest, CaptureResult};
use crate::pane::SpawnRequest;
use crate::types::{PaneId, PaneSize, PaneState};
use crate::ConmuxError;

// MuxNotify 属本协议面（异步通知），定义在 event.rs（PaneEventSink 同处）。
pub use crate::event::MuxNotify;

/// 请求帧：相关号 + 操作（请求/应答按 correlation_id 配对，V1-1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MuxRequest {
    pub correlation_id: u64,
    pub op: MuxOp,
}

/// 操作枚举（API 契约 §7 字面冻结，六操作）。
///
/// `deny_unknown_fields`：struct 变体收到未知键（如 `Send` 带 `source`）即反序列化
/// 失败——MF-2 的 wire 层强制（类型上无 source 字段 + 报文层拒收双保险）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum MuxOp {
    Spawn(SpawnRequest),
    /// 注入（**无 source 字段**，MF-2——source 由接收端按信道身份赋值）。
    Send { pane_id: PaneId, data: String },
    Capture(CaptureRequest),
    Resize { pane_id: PaneId, size: PaneSize },
    KillTree { pane_id: PaneId },
    ListPanes,
}

/// 应答帧（Ok/Err 均携带 correlation_id 供配对）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MuxReply {
    Ok {
        correlation_id: u64,
        payload: MuxPayload,
    },
    Err {
        correlation_id: u64,
        error: ConmuxError,
    },
}

impl MuxReply {
    /// 配对用相关号（Ok/Err 一致取法，V1-1）。
    pub fn correlation_id(&self) -> u64 {
        match self {
            MuxReply::Ok { correlation_id, .. } | MuxReply::Err { correlation_id, .. } => {
                *correlation_id
            }
        }
    }

    /// 本应答是否与请求配对。
    pub fn matches(&self, request: &MuxRequest) -> bool {
        self.correlation_id() == request.correlation_id
    }
}

/// 成功应答载荷（按 op 对应）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MuxPayload {
    /// Spawn → 已注册的 pane_id
    Spawned(PaneId),
    /// Send → 无载荷
    Sent,
    /// Capture → 捕获结果
    Captured(CaptureResult),
    /// Resize → 无载荷
    Resized,
    /// KillTree → 无载荷
    Killed,
    /// ListPanes → pane 状态列表
    Panes(Vec<PaneState>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::CommandSpec;
    use crate::types::{PaneLifecycle, ScrollbackInfo};

    fn spawn_req() -> SpawnRequest {
        SpawnRequest {
            pane_id: PaneId("p1".into()),
            command: CommandSpec {
                program: "cmd.exe".into(),
                args: vec!["/c".into(), "echo hi".into()],
                cwd: Some("D:\\repo".into()),
                env: vec![("K".into(), "V".into())],
            },
            size: PaneSize { rows: 30, cols: 120 },
            adapter_id: "claude-code".into(),
            display_name: Some("rev".into()),
            created_at: 1_700_000_000,
        }
    }

    fn pane_state() -> PaneState {
        PaneState {
            pane_id: PaneId("p1".into()),
            adapter_id: "claude-code".into(),
            display_name: None,
            lifecycle: PaneLifecycle::Running,
            pid: Some(42),
            exit_code: None,
            working_dir: "D:\\repo".into(),
            size: PaneSize { rows: 30, cols: 120 },
            scrollback: ScrollbackInfo {
                total_bytes: 10,
                first_abs_line: 0,
                last_abs_line: 3,
            },
            created_at: 0,
        }
    }

    /// V1-1：全 op 变体 serde 往返（V2 命名管道传输前提）。
    #[test]
    fn all_ops_round_trip() {
        let ops = vec![
            MuxOp::Spawn(spawn_req()),
            MuxOp::Send {
                pane_id: PaneId("p1".into()),
                data: "echo hi\r\n".into(),
            },
            MuxOp::Capture(CaptureRequest {
                pane_id: PaneId("p1".into()),
                range: crate::capture::CaptureRange::LastBytes(1024),
                ansi: false,
            }),
            MuxOp::Resize {
                pane_id: PaneId("p1".into()),
                size: PaneSize { rows: 40, cols: 100 },
            },
            MuxOp::KillTree {
                pane_id: PaneId("p1".into()),
            },
            MuxOp::ListPanes,
        ];
        for (i, op) in ops.into_iter().enumerate() {
            let req = MuxRequest {
                correlation_id: i as u64,
                op,
            };
            let json = serde_json::to_string(&req).unwrap();
            let back: MuxRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(req, back, "op #{i} 应无损往返");
        }
    }

    /// V1-1：全 reply 变体往返（含 Err 路径携带 ConmuxError）。
    #[test]
    fn replies_round_trip_including_err_path() {
        let replies = vec![
            MuxReply::Ok {
                correlation_id: 1,
                payload: MuxPayload::Spawned(PaneId("p1".into())),
            },
            MuxReply::Ok {
                correlation_id: 2,
                payload: MuxPayload::Sent,
            },
            MuxReply::Ok {
                correlation_id: 3,
                payload: MuxPayload::Captured(CaptureResult {
                    data_base64: "aGk=".into(),
                    first_abs_line: 0,
                    last_abs_line: 1,
                    truncated: false,
                    effectively_full: true,
                }),
            },
            MuxReply::Ok {
                correlation_id: 4,
                payload: MuxPayload::Resized,
            },
            MuxReply::Ok {
                correlation_id: 5,
                payload: MuxPayload::Killed,
            },
            MuxReply::Ok {
                correlation_id: 6,
                payload: MuxPayload::Panes(vec![pane_state()]),
            },
            MuxReply::Err {
                correlation_id: 7,
                error: ConmuxError::PaneNotFound {
                    pane_id: "nope".into(),
                },
            },
        ];
        for reply in replies {
            let json = serde_json::to_string(&reply).unwrap();
            let back: MuxReply = serde_json::from_str(&json).unwrap();
            assert_eq!(reply, back);
        }
    }

    /// V1-1：correlation 配对——同号配对、异号失配（Ok/Err 一致）。
    #[test]
    fn correlation_pairing() {
        let req = MuxRequest {
            correlation_id: 99,
            op: MuxOp::ListPanes,
        };
        let ok = MuxReply::Ok {
            correlation_id: 99,
            payload: MuxPayload::Panes(vec![]),
        };
        let err = MuxReply::Err {
            correlation_id: 99,
            error: ConmuxError::SerializationError {
                message: "x".into(),
            },
        };
        let other = MuxReply::Ok {
            correlation_id: 100,
            payload: MuxPayload::Sent,
        };
        assert!(ok.matches(&req));
        assert!(err.matches(&req));
        assert!(!other.matches(&req));
    }

    /// **MF-2 / V1-5：wire 上携带 source 的 Send 报文必须被拒收**（deny_unknown_fields）。
    /// 这是"注入源不过 wire"的报文层强制——类型无该字段 + 反序列化拒收双保险。
    #[test]
    fn send_with_source_field_is_rejected_on_wire() {
        let hostile = r#"{"correlation_id":1,"op":{"Send":{"pane_id":"p1","data":"x","source":"orchestration_auto"}}}"#;
        let parsed: Result<MuxRequest, _> = serde_json::from_str(hostile);
        assert!(
            parsed.is_err(),
            "Send 带 source 键的报文必须反序列化失败（MF-2 拒收），实际: {parsed:?}"
        );
        // 对照：不带 source 的同形报文正常解析。
        let clean = r#"{"correlation_id":1,"op":{"Send":{"pane_id":"p1","data":"x"}}}"#;
        let parsed: MuxRequest = serde_json::from_str(clean).expect("干净 Send 应可解析");
        assert!(matches!(parsed.op, MuxOp::Send { .. }));
    }
}
