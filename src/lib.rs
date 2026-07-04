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
//!
//! ## Stability（M1 契约增补 §1，两档承诺）
//!
//! - **承诺面（committed）**：0.x 期间变更需 **minor bump + CHANGELOG 条目**，patch 不得破坏。
//!   清单：protocol wire 类型（`MuxRequest`/`MuxOp`/`MuxPayload`/`MuxReply`/`MuxNotify`）及其
//!   wire 携带类型闭包（`SpawnRequest`/`CommandSpec`/`Capture*`/`PaneId`/`PaneSize`/`PaneState`/
//!   `PaneLifecycle`/`ScrollbackInfo`/`ConmuxError`）、`PaneHost` 门面全部 pub 方法、事件面
//!   （`PaneEventSink`）、注入扩展点（`InjectionHook`/`InjectionContext`/`InjectionSource`）、
//!   主题面（`TerminalTheme`/`ThemeAppearance`/`builtin_terminal_themes`/`DEFAULT_TERMINAL_THEME_ID`，
//!   附加语义：预置 id 永不复用改义、默认 id 变更 = minor）。
//!   行为语义随附冻结：唯一写链（MF-1）、钩子顺序（MF-6）、wire 拒收 source（MF-2）、
//!   `PaneOutput.seq` per-pane 从 1 起严格单调、kill 失败仍清表（MF-4 cl.4）、等效全量判定。
//! - **unstable 面**：标注 "Stability: unstable" 的项（`job` 模块四项等）may change without
//!   notice，patch 内可变。承诺面以 **crate 根 re-export 路径**为准，模块内路径不承诺。

// ===== V0 模块（按 conmux_api_contract.md 实现）=====

/// 机制层错误类型（契约 §1）。不依赖 conflux。
pub mod error;
/// 机制层类型：PaneId / PaneSize / PaneLifecycle / InjectionSource / PaneState 等（契约 §1/§8）。
pub mod types;
/// scrollback：行索引环形缓冲（契约 §5）。`pub(crate)` 内部——
/// 对外只经 capture / `PaneState.scrollback` 暴露语义化结果，不暴露缓冲本体。
pub(crate) mod scrollback;

/// VT 私有模式跟踪器（M2 重放架构）。`pub(crate)` 内部——
/// 对外只经 `PaneHost::mode_preamble` 暴露合成前导。
pub(crate) mod modes;

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

/// **Stability: unstable** — may change without notice.
/// 长度前缀帧编解码（M2 设计 D-4）：`u32 LE + JSON(WireFrame)`，4 MiB 上限。
/// daemon / 客户端 / 第三方前端共用的 wire 编解码面；M2 期可能调整。
pub mod wire;

/// Pane 抽象与 PaneHost 门面（契约 §2）。`PaneBackend`/`PaneSession`/`Pane` 为
/// `pub(crate)` 隐私墙（MF-1）；`PaneHost`/`CommandSpec`/`SpawnRequest` 对外公开。
pub mod pane;

/// Windows ConPTY 后端（cutover 2b-2，portable-pty 0.9 + DSR 应答）。仅 cfg(windows)。
/// `pub(crate)`（M1 契约 §1.3-②收紧）：消费方一律经 `PaneHost::new_windows` 装配，
/// 不直接触达 backend 类型。
#[cfg(windows)]
pub(crate) mod pane_win;

/// **Stability: unstable** — may change without notice。
/// 命名管道传输原语（M2a，仅 Windows）：服务端监听（FIRST_PIPE_INSTANCE + DACL +
/// REJECT_REMOTE，I-1..I-5）+ 客户端连接 + 单连接字节流。
#[cfg(windows)]
pub mod pipe;

/// **Stability: unstable** — may change without notice。
/// conmux daemon 服务端（M2a，仅 Windows）：管道监听 + 握手 + dispatcher（经 PaneHost）
/// + KillServer。承重墙安全不变量 I-2/I-5/R-1/R-2/H-2/H-3 的落实位。
#[cfg(windows)]
pub mod daemon;

/// **Stability: unstable** — may change without notice。
/// conmux 瘦客户端（M2a，仅 Windows）：连接（自动拉起）+ 握手 + 请求-应答。
#[cfg(windows)]
pub mod client;

/// 终端主题预置注册表（契约 D7：多预置 + 背景基调可调）。conmux 是主题数据
/// 唯一属主，conflux 与未来独立 CLI 共享。
pub mod theme;

/// 信任校验（Slice 2 · 安全本体）：WinVerifyTrust Authenticode 验签 + 无签名哈希钉 TOFU
/// + fail-closed 决策。注入 `PaneHost`（参照 hooks/event_sink 模式）。
pub mod trust;

// ===== 公开 API 重导出（顶层可达）=====
pub use capture::{CaptureRange, CaptureRequest, CaptureResult};
pub use error::ConmuxError;
pub use event::{MuxNotify, PaneEventSink};
pub use inject::{InjectionContext, InjectionHook};
pub use job::{ProcessSupervisor, SupervisorFactory};
#[cfg(windows)]
pub use job::{JobObjectSupervisor, JobObjectSupervisorFactory};
// PaneHost 类型 + 公开入参类型对外可见；构造器 2a 仍 pub(crate)（待 2b Windows 构造器）。
pub use pane::{CommandSpec, PaneHost, PaneSnapshot, SpawnRequest};
pub use protocol::{MuxOp, MuxPayload, MuxReply, MuxRequest, WireFrame, PROTOCOL_VERSION};
pub use wire::{read_frame, write_frame, WireError, MAX_FRAME_BYTES};
pub use theme::{builtin_terminal_themes, TerminalTheme, ThemeAppearance, DEFAULT_TERMINAL_THEME_ID};
// M③ Style 注册表（chrome 语义 token + 配对终端预置；TerminalTheme 结构未动）。
pub use theme::{builtin_styles, ChromeTokens, Style, DEFAULT_STYLE_ID};
pub use trust::{PinnedTarget, SharedTrustStore, TrustDecision, TrustMode, TrustPolicy, TrustStore};
pub use types::{InjectionSource, PaneId, PaneLifecycle, PaneSize, PaneState, ScrollbackInfo};
