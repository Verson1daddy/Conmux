//! Mux 协议类型冻结（API 契约 §7 / 总契约 §4.4 / MF-2 / M2 设计 D-4/D-8）。
//!
//! V1-core 批次冻结请求/应答类型 + serde 往返；**M2a 增补** daemon IPC 所需的全部
//! wire 形状——帧信封 [`WireFrame`]、握手 [`WireFrame::Hello`]/[`WireFrame::HelloAck`]、
//! daemon 命令面新 op（Respawn/Subscribe/Attach/ListThemes/SetTheme/KillServer）及其
//! 应答载荷。in-proc 消费者（conflux）仍直调 `PaneHost`；wire 类型经命名管道原样传输。
//!
//! ## 冻结不变量
//! - **MF-2：`MuxOp::Send` 无 `source` 字段**——`InjectionSource` 由接收端边界
//!   （`PaneHost::inject_stdin`）按信道身份赋值；本枚举启用 `deny_unknown_fields`，
//!   wire 上携带 `source` 键的 `Send` 报文**反序列化即失败**（拒收强于静默丢弃）。
//!   IPC 客户端注入一律映射 `UserDirect`（R-2，dispatcher 硬编码，wire 无协商面）。
//! - `MuxOp::Send` 的执行语义 = **必经** `PaneHost::inject_stdin`（注入钩子链），
//!   无其它实现路径（MF-1 / R-1）；daemon 的 dispatcher 实现受此约束。
//! - `MuxReply::Err` 携带 [`ConmuxError`]（机制层错误）——对总契约 §4.4 字面
//!   `ConfluxError` 的机制/策略分层修正（conmux 不依赖 conflux）；conflux 在 IPC
//!   边界经 `From<ConmuxError>` 转前端友好错误。
//! - 异步事件 [`MuxNotify`]（无 correlation）复用 `event.rs` 类型——其对总契约 §7
//!   字面的偏离（`data` 用原始 `Vec<u8>`、无 PaneStateChanged）见 event.rs 文档；
//!   wire 上 `data` 经 base64 适配（D-4，与 conflux 前端消费编码一致）。
//! - `PaneOutput.seq` 单调（重放对账），V1 起即填——由 PaneHost 读线程保证，
//!   e2e 断言见 pane.rs。
//! - **帧方向约束（D-4 / 红队 H-2）**：daemon 侧只接受 `Hello`（仅握手期）与 `Request`；
//!   客户端侧只接受 `HelloAck`/`Reply`/`Notify`。方向违例 = 协议错误断连。
//!   `WireFrame` 信封 + Hello/HelloAck 启用 `deny_unknown_fields` + 固定 externally
//!   tagged 表示——把 MuxNotify 补 Deserialize 扩张的解析面收回 MF-2 口径。
//! - **协议版本 [`PROTOCOL_VERSION`]** 独立于 crate 版本；握手 v1 严格相等校验。
//!
//! ## 承诺面纪律（M1 契约 §1.2）
//! 本模块全部 wire 类型为**承诺面**——serde 形状即跨进程协议，变更走 minor + CHANGELOG。
//! `MuxOp` / `MuxPayload` 新增变体本身即一次显式 minor 决策（`MuxOp` 不加 `non_exhaustive`：
//! dispatcher 必须穷尽处理，新增变体在 daemon 侧产生编译错误以强制裁决）。

use serde::{Deserialize, Serialize};

use crate::capture::{CaptureRequest, CaptureResult};
use crate::pane::SpawnRequest;
use crate::theme::TerminalTheme;
use crate::types::{PaneId, PaneSize, PaneState};
use crate::ConmuxError;

// MuxNotify 属本协议面（异步通知），定义在 event.rs（PaneEventSink 同处）。
pub use crate::event::MuxNotify;

/// IPC 协议版本（D-4）。**独立于 crate 版本**——握手 v1 要求客户端/daemon 严格相等。
/// 任何 wire 形状的破坏性变更必须 bump 本常量并在 CHANGELOG 登记。
pub const PROTOCOL_VERSION: u32 = 1;

/// 请求帧：相关号 + 操作（请求/应答按 correlation_id 配对，V1-1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MuxRequest {
    pub correlation_id: u64,
    pub op: MuxOp,
}

/// 操作枚举（API 契约 §7 + M2 设计 D-8 命令面）。
///
/// `deny_unknown_fields`：struct 变体收到未知键（如 `Send` 带 `source`）即反序列化
/// 失败——MF-2 的 wire 层强制（类型上无 source 字段 + 报文层拒收双保险）。
///
/// **不加 `#[non_exhaustive]`（M1 §1.3-④）**：变体集合本身是契约语义，新增变体应是
/// 显式 minor 决策；daemon dispatcher 对本枚举穷尽 match，未来加变体在 daemon 侧编译
/// 报错以强制裁决，而非静默忽略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum MuxOp {
    Spawn(SpawnRequest),
    /// 注入（**无 source 字段**，MF-2——source 由接收端按信道身份赋值）。
    /// `data` 为**原始字节**经 base64 上 wire（M2a-M2，与 `MuxNotify::PaneOutput.data` 口径统一）——
    /// raw attach（M2b/D-9）的方向键/Alt 组合/二进制粘贴非 UTF-8，String 无法无损携带。
    Send {
        pane_id: PaneId,
        #[serde(with = "crate::event::serde_b64")]
        data: Vec<u8>,
    },
    Capture(CaptureRequest),
    Resize { pane_id: PaneId, size: PaneSize },
    KillTree { pane_id: PaneId },
    ListPanes,
    // ===== M2a 增补（D-8 协议增补清单）=====
    /// 原子同 ID 重起（G-1，消 KillTree+Spawn 组合的 ID 复用窗口）。→ [`MuxPayload::Spawned`]
    Respawn(SpawnRequest),
    /// 按 pane 订阅事件流（D-5）。订阅者方收该 pane 的 `PaneOutput`/`PaneExited`。
    /// **fan-out 实现归 M2b**；M2a 类型冻结、dispatcher 返回 [`ConmuxError::Unsupported`]。
    Subscribe { pane_id: PaneId },
    /// 取消订阅（D-5）。处置同 `Subscribe`（M2b）。
    Unsubscribe { pane_id: PaneId },
    /// 原子「订阅 + 快照」（D-6）。→ [`MuxPayload::AttachSnapshot`]。
    /// **无缝拼接实现归 M2b**；M2a 类型冻结、dispatcher 返回 [`ConmuxError::Unsupported`]。
    Attach { pane_id: PaneId },
    /// 列主题预置（G-2）。→ [`MuxPayload::Themes`]
    ListThemes,
    /// 热切换主题（G-2）。→ [`MuxPayload::ThemeSet`]，并广播 [`MuxNotify::ThemeChanged`]。
    /// daemon **不持久化**偏好（持久化归消费者 / GUI 壳 M3）。
    SetTheme { id: String },
    /// 显式终结 daemon 及全部会话（D-2，先 kill 全部 pane 再退出）。→ [`MuxPayload::ServerKillScheduled`]
    KillServer,
    /// pin 一个可执行文件到信任库（Slice 3：让 daemon 内存态即时生效，免重启）。
    /// → [`MuxPayload::Pinned`]。daemon 调 `SharedTrustStore::pin_executable`（同 Arc，
    /// 下次 spawn verify 即见新 pin）+ 存盘。path 必须绝对路径。
    PinExecutable { path: String },
    /// 移除 pin（P1-b 2026-07-02：与 Pin 对称——此前 unpin 只直写文件，运行中 daemon
    /// 内存态不受影响，收权慢于授权）。→ [`MuxPayload::Unpinned`]。daemon 调
    /// `SharedTrustStore::unpin`（同 Arc，即时生效）+ 存盘。加法性变体，不 bump
    /// PROTOCOL_VERSION（沿 PinExecutable 先例；旧 daemon 收到未知变体 → 解码错断连，
    /// 客户端回退直写文件）。
    UnpinExecutable { path: String },
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
#[non_exhaustive] // M1 契约 §1.3-④
pub enum MuxPayload {
    /// Spawn / Respawn → 已注册的 pane_id
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
    // ===== M2a 增补 =====
    /// Subscribe → 订阅确认
    Subscribed,
    /// Unsubscribe → 取消确认
    Unsubscribed,
    /// Attach → 原子快照（D-6）：模式前导 + scrollback 历史（均 base64）+ 末序号 + 状态。
    /// 客户端重建 = 喂 preamble → 喂 history → 按 `seq > last_seq` 连续喂 live 流。
    AttachSnapshot {
        mode_preamble_b64: String,
        history_b64: String,
        last_seq: u64,
        pane_state: PaneState,
    },
    /// ListThemes → 主题预置列表
    Themes(Vec<TerminalTheme>),
    /// SetTheme → 切换确认（变更经 [`MuxNotify::ThemeChanged`] 广播）
    ThemeSet,
    /// KillServer → 终结已排程（daemon 随即 kill 全部 pane 并退出）
    ServerKillScheduled,
    /// PinExecutable → pin 成功（无载荷；失败走 `MuxReply::Err`）。
    Pinned,
    /// UnpinExecutable → 移除成功（无载荷；失败走 `MuxReply::Err`）。
    Unpinned,
}

/// IPC 帧信封（D-4 / 红队 H-2）。daemon 与客户端在同一连接上交换的全部帧。
///
/// **方向约束**（解析后由 daemon/client 强制，见模块文档）：
/// - daemon 只处理 `Hello`（仅握手期）与 `Request`；收到其它方向帧 → 协议错误断连。
/// - 客户端只处理 `HelloAck`/`Reply`/`Notify`；收到 `Request`/握手后再收 `Hello` → 断连。
///
/// `deny_unknown_fields` + externally tagged：恶意构造的多余字段被拒，把 MuxNotify
/// 补 Deserialize 扩张的解析面收回 MF-2 口径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum WireFrame {
    /// 客户端连接后**首帧必须** Hello（D-4）。`client_kind` 是自由标签仅入审计日志，
    /// **不参与任何授权判定**（I-6）。
    Hello {
        protocol_version: u32,
        client_kind: String,
    },
    /// daemon 对合法 Hello 的应答（版本匹配后）。
    HelloAck {
        protocol_version: u32,
        daemon_version: String,
    },
    /// 客户端 → daemon 请求。
    Request(MuxRequest),
    /// daemon → 客户端应答。
    Reply(MuxReply),
    /// daemon → 客户端异步事件（无 correlation）。
    Notify(MuxNotify),
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

    /// 全 op 变体 serde 往返（命名管道传输前提；含 M2a 新增命令面）。
    #[test]
    fn all_ops_round_trip() {
        let ops = vec![
            MuxOp::Spawn(spawn_req()),
            MuxOp::Send {
                pane_id: PaneId("p1".into()),
                data: b"echo hi\r\n".to_vec(),
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
            // M2a 增补
            MuxOp::Respawn(spawn_req()),
            MuxOp::Subscribe {
                pane_id: PaneId("p1".into()),
            },
            MuxOp::Unsubscribe {
                pane_id: PaneId("p1".into()),
            },
            MuxOp::Attach {
                pane_id: PaneId("p1".into()),
            },
            MuxOp::ListThemes,
            MuxOp::SetTheme {
                id: "b-dark-ink".into(),
            },
            MuxOp::KillServer,
            MuxOp::PinExecutable {
                path: "C:\\shim\\evil.cmd".into(),
            },
            MuxOp::UnpinExecutable {
                path: "C:\\shim\\evil.cmd".into(),
            },
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
        // data 现为 base64（M2a-M2）；"eA==" = "x"。
        let hostile = r#"{"correlation_id":1,"op":{"Send":{"pane_id":"p1","data":"eA==","source":"orchestration_auto"}}}"#;
        let parsed: Result<MuxRequest, _> = serde_json::from_str(hostile);
        assert!(
            parsed.is_err(),
            "Send 带 source 键的报文必须反序列化失败（MF-2 拒收），实际: {parsed:?}"
        );
        // 对照：不带 source 的同形报文正常解析，data base64 解码回原始字节。
        let clean = r#"{"correlation_id":1,"op":{"Send":{"pane_id":"p1","data":"eA=="}}}"#;
        let parsed: MuxRequest = serde_json::from_str(clean).expect("干净 Send 应可解析");
        match parsed.op {
            MuxOp::Send { data, .. } => assert_eq!(data, b"x"),
            other => panic!("应为 Send，实际 {other:?}"),
        }
    }

    /// M2a：新增应答载荷变体 serde 往返（含 AttachSnapshot / Themes）。
    #[test]
    fn m2a_payloads_round_trip() {
        let themes = crate::theme::builtin_terminal_themes();
        assert!(!themes.is_empty(), "内置主题非空");
        let payloads = vec![
            MuxPayload::Subscribed,
            MuxPayload::Unsubscribed,
            MuxPayload::AttachSnapshot {
                mode_preamble_b64: "G1s/MTA0OWg=".into(),
                history_b64: "aGVsbG8=".into(),
                last_seq: 42,
                pane_state: pane_state(),
            },
            MuxPayload::Themes(themes),
            MuxPayload::ThemeSet,
            MuxPayload::ServerKillScheduled,
            MuxPayload::Pinned,
        ];
        for (i, payload) in payloads.into_iter().enumerate() {
            let reply = MuxReply::Ok {
                correlation_id: i as u64,
                payload,
            };
            let json = serde_json::to_string(&reply).unwrap();
            let back: MuxReply = serde_json::from_str(&json).unwrap();
            assert_eq!(reply, back, "payload #{i} 应无损往返");
        }
    }

    /// D-4：WireFrame 信封全方向往返（Hello/HelloAck/Request/Reply/Notify）。
    #[test]
    fn wire_frame_all_directions_round_trip() {
        let frames = vec![
            WireFrame::Hello {
                protocol_version: PROTOCOL_VERSION,
                client_kind: "conmux-cli".into(),
            },
            WireFrame::HelloAck {
                protocol_version: PROTOCOL_VERSION,
                daemon_version: "0.1.0".into(),
            },
            WireFrame::Request(MuxRequest {
                correlation_id: 1,
                op: MuxOp::ListPanes,
            }),
            WireFrame::Reply(MuxReply::Ok {
                correlation_id: 1,
                payload: MuxPayload::Panes(vec![pane_state()]),
            }),
            WireFrame::Notify(MuxNotify::PaneOutput {
                pane_id: PaneId("p1".into()),
                seq: 7,
                data: b"\x1b[31mred\x1b[0m".to_vec(),
            }),
            WireFrame::Notify(MuxNotify::ThemeChanged {
                id: "b-dark-ink".into(),
            }),
        ];
        for (i, frame) in frames.into_iter().enumerate() {
            let json = serde_json::to_string(&frame).unwrap();
            let back: WireFrame = serde_json::from_str(&json).unwrap();
            assert_eq!(frame, back, "frame #{i} 应无损往返");
        }
    }

    /// D-4：MuxNotify.data 经 base64 过 wire（与 conflux 前端消费编码一致）。
    #[test]
    fn pane_output_data_is_base64_on_wire() {
        let frame = WireFrame::Notify(MuxNotify::PaneOutput {
            pane_id: PaneId("p1".into()),
            seq: 1,
            data: vec![0x00, 0x1b, 0xff], // 含不可打印/非 UTF-8 字节
        });
        let json = serde_json::to_string(&frame).unwrap();
        // \x00\x1b\xff → base64 "ABv/"
        assert!(json.contains("ABv/"), "data 应 base64 编码上 wire: {json}");
        let back: WireFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(frame, back);
    }

    /// D-4 / H-2：Hello 帧 `deny_unknown_fields`——多余字段被拒收。
    #[test]
    fn hello_rejects_unknown_fields() {
        let clean = r#"{"Hello":{"protocol_version":1,"client_kind":"cli"}}"#;
        assert!(serde_json::from_str::<WireFrame>(clean).is_ok());
        let hostile = r#"{"Hello":{"protocol_version":1,"client_kind":"cli","escalate":"admin"}}"#;
        assert!(
            serde_json::from_str::<WireFrame>(hostile).is_err(),
            "Hello 带未知字段必须拒收（H-2）"
        );
    }
}
