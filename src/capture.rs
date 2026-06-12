//! capture：ANSI 开关捕获（API 契约 §6）。
//!
//! 提供 scrollback 的按需 dump，含两条机制层不变量：
//! - **ANSI 开关**：`ansi=false` 时剥离 VT 控制序列（喂 LLM / 搜索）；`true` 保留原始 VT。
//! - **等效全量触发 read 审计（复闸 C2）**：按**有效覆盖判定、不按枚举变体**——
//!   `All` / `LastBytes(n ≥ 有效字节)` / `LineRange` 覆盖 ≥ 可读行窗 80% 都算"等效全量"，
//!   杜绝换 range 变体规避审计。判定函数 [`is_effectively_full`] 纯函数、可测；
//!   `PaneHost::capture` 把判定结果放进 `CaptureResult.effectively_full`，
//!   read 审计的写入归 conflux 策略层（审计存储不在机制层）。
//!
//! 完整的 `capture_from_buffer`（读字节 + base64 组装 `CaptureResult`）在 PaneHost
//! 增量接 `LineIndexedBuffer` 时落地；本增量先冻结类型 + 两条纯逻辑。

// V0 增量：纯函数在被 PaneHost::capture 接线前为 dead；接线后移除。
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::types::PaneId;

/// 捕获请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRequest {
    pub pane_id: PaneId,
    pub range: CaptureRange,
    /// false = 剥离后喂 LLM / 搜索；true = 原始 VT。
    pub ansi: bool,
}

/// 捕获范围。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureRange {
    All,
    LastBytes(usize),
    LineRange { start_abs: u64, end_abs: u64 },
}

/// 捕获结果。`data_base64` 为编码后的字节（ansi 开关已应用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureResult {
    pub data_base64: String,
    pub first_abs_line: u64,
    pub last_abs_line: u64,
    /// ring 覆盖导致请求范围部分不可得。
    pub truncated: bool,
    /// 等效全量 dump（复闸 C2，[`is_effectively_full`] 按有效覆盖判定）。
    /// 机制层只判定不存储；conflux 据此写 `CaptureDump` read 审计。
    pub effectively_full: bool,
}

/// 等效全量判定（复闸 C2，冻结）——按**有效覆盖**而非枚举变体判定。
///
/// 返回 true 表示该请求等效于 dump 全部 scrollback，`PaneHost::capture` 据此触发
/// read 审计钩子（防止用 `LastBytes(usize::MAX)` 或超宽 `LineRange` 规避审计）。
///
/// - `valid_bytes`：ring 内当前有效字节数。
/// - `(avail_first, avail_last)`：当前可读行窗（`LineIndexedBuffer::line_range_available`）。
pub(crate) fn is_effectively_full(
    range: &CaptureRange,
    valid_bytes: usize,
    avail_first: u64,
    avail_last: u64,
) -> bool {
    match range {
        CaptureRange::All => true,
        CaptureRange::LastBytes(n) => *n >= valid_bytes,
        CaptureRange::LineRange { start_abs, end_abs } => {
            // 覆盖 ≥ 当前可读行窗 80% 视为等效全量。
            let window = avail_last.saturating_sub(avail_first).max(1);
            let covered = end_abs.saturating_sub(*start_abs);
            covered.saturating_mul(100) >= window.saturating_mul(80)
        }
    }
}

/// 剥离 VT 控制序列（基础版：CSI / OSC / 单字符 ESC 序列）。
///
/// 用于 `ansi=false` 捕获——喂 LLM / 搜索时去噪。保留可打印字节与 `\n`/`\t` 等。
/// V0 基础实现覆盖最常见序列；如需完备解析可后续替换为 `vte` 级状态机
/// （契约 §6：用独立纯函数，不复用 parser 状态机——职责不同）。
pub(crate) fn strip_ansi(input: &[u8]) -> Vec<u8> {
    const ESC: u8 = 0x1b;
    const BEL: u8 = 0x07;
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b != ESC {
            out.push(b);
            i += 1;
            continue;
        }
        // ESC 序列：看下一字节判类型。
        match input.get(i + 1) {
            Some(b'[') => {
                // CSI：ESC [ ... 终止于 0x40..=0x7e 的字节。
                i += 2;
                while i < input.len() && !(0x40..=0x7e).contains(&input[i]) {
                    i += 1;
                }
                i += 1; // 跳过终止字节
            }
            Some(b']') => {
                // OSC：ESC ] ... 终止于 BEL 或 ST(ESC \)。
                i += 2;
                while i < input.len() {
                    if input[i] == BEL {
                        i += 1;
                        break;
                    }
                    if input[i] == ESC && input.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            Some(_) => {
                // 其它 ESC 序列（如 ESC ( B）：跳过 ESC + 下一字节（基础处理）。
                i += 2;
            }
            None => {
                // 孤立结尾 ESC：丢弃。
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_csi_color_codes() {
        let input = b"\x1b[1;31mERROR\x1b[0m done";
        assert_eq!(strip_ansi(input), b"ERROR done");
    }

    #[test]
    fn strip_ansi_removes_osc_with_bel_and_st() {
        // OSC 以 BEL 结束
        assert_eq!(strip_ansi(b"\x1b]0;title\x07text"), b"text");
        // OSC 以 ST (ESC \) 结束
        assert_eq!(strip_ansi(b"\x1b]11;rgb:00/00/00\x1b\\X"), b"X");
    }

    #[test]
    fn strip_ansi_keeps_plain_text_and_newlines() {
        assert_eq!(strip_ansi(b"line1\nline2\t!"), b"line1\nline2\t!");
    }

    #[test]
    fn strip_ansi_handles_trailing_lone_esc() {
        assert_eq!(strip_ansi(b"abc\x1b"), b"abc");
    }

    #[test]
    fn effectively_full_for_all_variant() {
        assert!(is_effectively_full(&CaptureRange::All, 100, 0, 10));
    }

    #[test]
    fn effectively_full_for_lastbytes_ge_valid() {
        assert!(is_effectively_full(&CaptureRange::LastBytes(100), 100, 0, 10));
        assert!(is_effectively_full(
            &CaptureRange::LastBytes(usize::MAX),
            100,
            0,
            10
        ));
        // 小于有效字节 → 非等效全量
        assert!(!is_effectively_full(&CaptureRange::LastBytes(50), 100, 0, 10));
    }

    #[test]
    fn effectively_full_for_wide_line_range() {
        // 可读窗 [0,10] = 10 行；覆盖 9 行（90%）→ 等效全量
        assert!(is_effectively_full(
            &CaptureRange::LineRange { start_abs: 0, end_abs: 9 },
            1000,
            0,
            10
        ));
        // 覆盖 3 行（30%）→ 非等效全量
        assert!(!is_effectively_full(
            &CaptureRange::LineRange { start_abs: 2, end_abs: 5 },
            1000,
            0,
            10
        ));
    }

    #[test]
    fn capture_request_round_trips() {
        let req = CaptureRequest {
            pane_id: PaneId("p1".into()),
            range: CaptureRange::LineRange { start_abs: 5, end_abs: 20 },
            ansi: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CaptureRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }
}
