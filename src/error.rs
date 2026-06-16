//! conmux 机制层错误类型（API 契约 §1）。
//!
//! conmux **不依赖 conflux**——conflux 侧用 `From<ConmuxError>` 在 IPC 边界
//! 转为前端友好错误。本类型只覆盖机制层失败（spawn / PTY / 监管 / 注入 / 运行时），
//! **不含** conflux 策略层概念（讨论、适配器注册、窗口、DB schema 等）。

use serde::{Deserialize, Serialize};

/// conmux 机制层统一错误（`MuxReply::Err` 携带它，故需 serde 可序列化）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[non_exhaustive] // M1 契约 §1.3-④：未来加变体不破坏调用方（变体新增仍走 minor+changelog）
pub enum ConmuxError {
    /// 目标 pane 不存在（对应现状 `ConfluxError::InstanceNotFound`）。
    #[error("pane 不存在: {pane_id}")]
    PaneNotFound { pane_id: String },

    /// spawn 失败（含 RuntimeAdapter 命令构造 / backend.open+spawn 失败）。
    #[error("spawn 失败: {message}")]
    SpawnFailed { message: String },

    /// PTY 读写错误。
    #[error("PTY 错误: {message}")]
    PtyError { message: String },

    /// 进程监管错误（JobObject assign / kill_tree 失败，MF-4）。
    #[error("进程监管错误: {message}")]
    SupervisorError { message: String },

    /// 注入被 InjectionHook 链拒绝（policy 闸 / 限速 / 审计 fail-closed，MF-5/MF-6）。
    /// 这是 conmux 的机制语义——具体拒绝原因由 conflux 钩子提供。
    #[error("注入被拒绝: {reason}")]
    InjectionRejected { reason: String },

    /// 运行时接入错误（RuntimeAdapter：路径翻译 / 退出码映射等，D9）。
    #[error("运行时接入错误: {message}")]
    RuntimeError { message: String },

    /// 序列化/反序列化错误（协议层）。
    #[error("序列化错误: {message}")]
    SerializationError { message: String },

    /// op 被识别但此 daemon 构建不支持（M2a：Subscribe/Attach 等分阶段到 M2b；
    /// 或客户端比 daemon 新、请求了未实现的操作）。区别于 `SerializationError`
    /// （后者是 wire 解析失败，前者是语义上能解析但 daemon 无对应实现）。
    #[error("不支持的操作: {message}")]
    Unsupported { message: String },

    /// 资源临时不可用，请稍后重试（M2b：attach 限速 / per-pane 并发快照=1 的排队拒绝，D-7）。
    #[error("资源繁忙: {message}")]
    Busy { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_context() {
        let e = ConmuxError::PaneNotFound {
            pane_id: "pane-7".into(),
        };
        assert_eq!(e.to_string(), "pane 不存在: pane-7");
    }

    #[test]
    fn error_is_serde_round_trippable() {
        let e = ConmuxError::InjectionRejected {
            reason: "rate limit".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: ConmuxError = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
