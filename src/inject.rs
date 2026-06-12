//! 注入下沉：库级唯一注入路径的钩子扩展点（API 契约 §4 / MF-1/2/3/5/6）。
//!
//! conmux 的核心差异化机制——把控制面"唯一注入路径"从项目约定**下沉为库级不变量**：
//! 所有 send 经 `PaneHost::inject_stdin` 时按注册顺序触发 [`InjectionHook`]；
//! `before_inject` 全部通过才写 PTY，任一返回 Err ⇒ fail-closed 不注入（MF-6）。
//!
//! conmux 只定义 trait + 保证调用顺序，**不实现存储**——审计落库 / policy 闸 /
//! 限速值都是 conflux 钩子实现（机制/策略分层）。

use crate::types::{InjectionSource, PaneId};
use crate::ConmuxError;

/// conmux 赋值、传给钩子的上下文。`source` 由信道身份赋值（MF-2），钩子只读。
#[non_exhaustive]
pub struct InjectionContext<'a> {
    /// 目标 pane（MF-3：使限速钩子能按 per-pane 计数）。
    pub pane_id: &'a PaneId,
    /// 注入来源——由 conmux 在 `inject_stdin` 边界按**信道身份**赋值，不收调用方/wire 入参。
    pub source: InjectionSource,
    /// 注入字节数。
    pub byte_len: usize,
    /// 注入内容（钩子可读以做内容策略 / 审计 payload）。
    pub content: &'a [u8],
}

impl<'a> InjectionContext<'a> {
    /// 构造上下文（`byte_len` 派生自 `content`）。生产路径仅 `PaneHost::inject_stdin`
    /// 构造；公开此构造器供消费方为自己的 `InjectionHook` 实现写单测（`#[non_exhaustive]`
    /// 否则挡住外部字面量构造）。
    pub fn new(pane_id: &'a PaneId, source: InjectionSource, content: &'a [u8]) -> Self {
        Self {
            pane_id,
            source,
            byte_len: content.len(),
            content,
        }
    }
}

/// 注入钩子。所有 send 经 `PaneHost::inject_stdin` 时按注册顺序触发。
///
/// **conmux 保证（顺序不变量，MF-6）**：`before_inject`（全部钩子）→ `session.write_all`
/// → `after_inject`。任一 `before_inject` 返回 Err ⇒ 字节**绝不**抵达 PTY。
pub trait InjectionHook: Send + Sync {
    /// 字节抵达 PTY **之前**调用。返回 Err ⇒ 中止注入（fail-closed）。
    /// 用于：限速（MF-3，按 `ctx.pane_id`）、policy 闸（MF-5）、审计 commit（MF-6）。
    fn before_inject(&self, ctx: &InjectionContext) -> Result<(), ConmuxError>;

    /// 字节写入 PTY **之后**调用（含成功/失败结果）。用于追加 Failed 审计等。
    /// 默认 no-op——只关心放行判定的钩子无需实现。
    fn after_inject(&self, _ctx: &InjectionContext, _result: &Result<(), ConmuxError>) {}
}
