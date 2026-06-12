//! conmux 机制层类型（API 契约 §1 / §8）。
//!
//! 全部 `serde` 可序列化（协议类型 `MuxNotify`/`PaneState` 经命名管道 V2 预留）。
//! **机制 vs 策略边界**：本模块只携带**进程级机制状态**（`PaneLifecycle`）；
//! "思考中 / 等权限"等对 PTY 内容的**语义解读**属 conflux 策略层，不进 conmux
//! （Red Team 第三轮裁决②：机制库不携带 agent 语义状态）。

use serde::{Deserialize, Serialize};

/// Pane 标识。`PaneId == conflux InstanceId` 别名（总契约 §1 "V1 不改 ID 体系"）——
/// 两名指同一 `String` newtype。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneId(pub String);

/// 终端尺寸（行 × 列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSize {
    pub rows: u16,
    pub cols: u16,
}

/// **机制层**进程生命周期状态（Red Team 第三轮裁决②）。
///
/// conmux 只拥有进程级状态；`AgentStatus`（Idle/Thinking/Coding/WaitingPermission/…）
/// 那种对 PTY 内容的语义解读由 conflux 据事件推断，**不进 conmux**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneLifecycle {
    /// 已请求 spawn、进程尚未确认运行（含 CREATE_SUSPENDED→assign→resume 窗口）。
    Spawning,
    /// 进程运行中。
    Running,
    /// 进程已退出（携退出码；WSL/SSH relay 下可能非真实远端码，见 RuntimeAdapter 映射）。
    Exited(i32),
    /// `kill_tree` 失败的残留态（MF-4 第 4 条：标记上报、调用方仍清理内部表）。
    Zombie,
}

/// 注入来源分类（机制层）。**MF-2**：由 conmux 在 `inject_stdin` 边界按**信道身份**
/// 赋值，绝不来自 wire / 调用方入参。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectionSource {
    /// 用户在 UI 直接输入（展开终端打字 / reply / send-to）。
    UserDirect,
    /// 权限确认响应（approve/deny 的 Y/N 注入）。
    PermissionResponse,
    /// 编排自动调度指令（需经 conflux 确认闸钩子，MF-5 / 控制面 §13.3）。
    OrchestrationAuto,
    /// 讨论中用户手动发送的消息。
    DiscussionUserMessage,
}

/// scrollback 元信息——经 `PaneState` 暴露语义，不暴露缓冲本体（契约 §5/§8）。
/// `abs_line` 为写入侧物理行号（不等于 xterm 视口行，坐标系消歧在 conflux）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrollbackInfo {
    pub total_bytes: u64,
    pub first_abs_line: u64,
    pub last_abs_line: u64,
}

/// pane 结构化状态（对账 / 死亡检测；契约 §8）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneState {
    pub pane_id: PaneId,
    pub adapter_id: String,
    pub display_name: Option<String>,
    /// 机制层生命周期（替 AgentStatus，裁决②）；语义状态由 conflux 另行投影。
    pub lifecycle: PaneLifecycle,
    pub pid: Option<u32>,
    /// `ProcessExited` 后必填；RuntimeAdapter 映射不可得时为 None（不静默伪装，D9）。
    pub exit_code: Option<i32>,
    /// **仅展示用途**（控制面 §13.8：不作任何 open/exec 入参）。
    pub working_dir: String,
    pub size: PaneSize,
    pub scrollback: ScrollbackInfo,
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injection_source_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&InjectionSource::OrchestrationAuto).unwrap(),
            "\"orchestration_auto\""
        );
        assert_eq!(
            serde_json::to_string(&InjectionSource::DiscussionUserMessage).unwrap(),
            "\"discussion_user_message\""
        );
    }

    #[test]
    fn pane_lifecycle_exited_carries_code() {
        let json = serde_json::to_string(&PaneLifecycle::Exited(-1)).unwrap();
        let back: PaneLifecycle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, PaneLifecycle::Exited(-1));
        // 运行态无 payload
        assert_eq!(
            serde_json::to_string(&PaneLifecycle::Running).unwrap(),
            "\"running\""
        );
    }

    #[test]
    fn pane_state_round_trips() {
        let st = PaneState {
            pane_id: PaneId("pane-1".into()),
            adapter_id: "codex".into(),
            display_name: Some("reviewer".into()),
            lifecycle: PaneLifecycle::Running,
            pid: Some(4242),
            exit_code: None,
            working_dir: "D:\\repo".into(),
            size: PaneSize { rows: 40, cols: 120 },
            scrollback: ScrollbackInfo {
                total_bytes: 2048,
                first_abs_line: 3,
                last_abs_line: 57,
            },
            created_at: 1_700_000_000,
        };
        let json = serde_json::to_string(&st).unwrap();
        let back: PaneState = serde_json::from_str(&json).unwrap();
        assert_eq!(st, back);
    }
}
