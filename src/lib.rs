//! # conmux
//!
//! Windows 原生终端多路复用核心 + agent 隔离运行时接入（**机制层**）。
//! Conflux 工作台（策略/产品层）基于本 crate 构建；依赖单向 `conflux → conmux`。
//! conmux **不依赖 Tauri**、不感知上层 UI / 业务概念（注意力队列、灵动岛等）。
//!
//! - 总契约：`.workbench/coordination/handoffs/F1_mux_contract.md`
//! - API 契约：`.workbench/coordination/handoffs/conmux_api_contract.md`
//!
//! ## 模块规划（V0 起按 API 契约逐步迁入，当前为骨架占位）
//! - `pane`       —— PaneBackend / PaneSession / PaneHost（retrofit 自 pty/manager.rs）
//! - `job`        —— ProcessSupervisor / JobObjectSupervisor（整树终结）
//! - `scrollback` —— LineIndexedBuffer（升级自 pty/buffer.rs）
//! - `capture`    —— ANSI 开关捕获
//! - `protocol`   —— MuxRequest / MuxReply / MuxNotify
//! - `inject`     —— InjectionHook（库级唯一注入路径，无旁路）
//! - `runtime`    —— RuntimeAdapter（local / wsl / docker / vm / ssh）
//! - `theme`      —— base24 色彩 schema 基板
//!
//! ## 安全不变量（契约 §13 / Red Team MF-1..6）
//! 唯一注入路径（writer 私有，无旁路）、InjectionSource 不过 wire（按信道身份赋值）、
//! per-instance 限速、JobObject fail-closed、审计钩子先于字节抵达 PTY。

// ===== V0 模块（按 conmux_api_contract.md 实现）=====

/// 机制层错误类型（契约 §1）。不依赖 conflux。
pub mod error;
/// 机制层类型：PaneId / PaneSize / PaneLifecycle / InjectionSource / PaneState 等（契约 §1/§8）。
pub mod types;

/// scrollback：行索引环形缓冲（契约 §5）。`pub(crate)` 内部——
/// 对外只经 capture / `PaneState.scrollback` 暴露语义化结果，不暴露缓冲本体。
pub(crate) mod scrollback;

/// capture：ANSI 开关捕获 + 等效全量审计判定（契约 §6）。
pub mod capture;

/// 注入下沉：InjectionHook / InjectionContext（库级唯一注入路径，契约 §4 / MF-1/5/6）。
pub mod inject;

/// Pane 事件出口：MuxNotify / PaneEventSink（契约 §9，conmux 不依赖 Tauri）。
pub mod event;

/// Mux 协议类型冻结：MuxRequest/MuxReply/MuxOp/MuxPayload（契约 §7 / §4.4，V1 仅
/// 类型 + serde，V2 命名管道原样传输；MF-2：Send 无 source 且 wire 拒收）。
pub mod protocol;

/// 进程监管：ProcessSupervisor / SupervisorFactory（每 pane 一 Job，契约 §3 / MF-4）。
pub mod job;

/// Pane 抽象与 PaneHost 门面（契约 §2）。`PaneBackend`/`PaneSession`/`Pane` 为
/// `pub(crate)` 隐私墙（MF-1）；`PaneHost`/`CommandSpec`/`SpawnRequest` 对外公开。
pub mod pane;

/// Windows ConPTY 后端（cutover 2b-2，portable-pty 0.9 + DSR 应答）。仅 cfg(windows)。
#[cfg(windows)]
pub mod pane_win;

/// 终端主题预置注册表（契约 D7：多预置 + 背景基调可调）。conmux 是主题数据
/// 唯一属主，conflux 与未来独立 CLI 共享。
pub mod theme;

// ===== 公开 API 重导出（顶层可达）=====
pub use capture::{CaptureRange, CaptureRequest, CaptureResult};
pub use error::ConmuxError;
pub use event::{MuxNotify, PaneEventSink};
pub use inject::{InjectionContext, InjectionHook};
pub use job::{ProcessSupervisor, SupervisorFactory};
#[cfg(windows)]
pub use job::{JobObjectSupervisor, JobObjectSupervisorFactory};
#[cfg(windows)]
pub use pane_win::WindowsPaneBackend;
// PaneHost 类型 + 公开入参类型对外可见；构造器 2a 仍 pub(crate)（待 2b Windows 构造器）。
pub use pane::{CommandSpec, PaneHost, SpawnRequest};
pub use protocol::{MuxOp, MuxPayload, MuxReply, MuxRequest};
pub use theme::{builtin_terminal_themes, TerminalTheme, ThemeAppearance, DEFAULT_TERMINAL_THEME_ID};
pub use types::{InjectionSource, PaneId, PaneLifecycle, PaneSize, PaneState, ScrollbackInfo};
