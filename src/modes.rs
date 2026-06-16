//! VT 私有模式跟踪器（M2 重放架构第一块砖）。
//!
//! Spike 裁决（research/conmux-positioning-2026-06-12/vt-replay-spike-2026-06-13.md）：
//! 屏幕文本/光标在 ring 任意起点重放下**自愈**（TUI 绝对定位重绘 + xterm 解析器
//! 优雅重同步），唯一不自愈的是**模态状态**（alt-screen / 光标可见性 / 鼠标 /
//! bracketed paste / 应用光标键）。故 attach 重放 = 「合成模式前导 + ring 字节」，
//! 不做服务端全网格状态机。
//!
//! 本跟踪器在读泵路径上增量扫描输出流的 DECSET/DECRST（`CSI ? Pm h|l`），
//! **必须容忍序列跨 chunk 撕裂**（读泵按任意边界喂 chunk）。
//!
//! 已知边界（spike 记录）：前导反映**流末尾**的模式状态；"模式在 ring 窗口内被
//! 复位、但置位发生在窗口之前"的罕见序错位场景下，窗口前段内容会以错误模式重放
//! （与现状 conflux 重放同级缺陷，不劣化）。tmux 级修复需 ring 起点态双跟踪，
//! 登记为 M2 可选增强。

/// 跨 chunk 的扫描状态机。只识别 `ESC [ ? <params> h|l`，其余序列直接放行。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ScanState {
    Ground,
    /// 已见 ESC。
    Esc,
    /// 已进 CSI。`private` = 已见 `?` 前缀；params 为已收集参数。
    Csi {
        private: bool,
        params: Vec<u16>,
        cur: u16,
        has_cur: bool,
    },
}

/// 私有模式跟踪器（per-pane，与 scrollback 同生命周期；respawn 即新建）。
#[derive(Debug)]
pub(crate) struct ModeTracker {
    /// 当前激活的 alt-screen 族模式号（1049/1047/47），None = 主屏。
    alt_screen: Option<u16>,
    /// `?25l` 光标隐藏（默认可见）。
    cursor_hidden: bool,
    /// `?1h` DECCKM 应用光标键（影响方向键编码，不被重绘自愈）。
    app_cursor_keys: bool,
    /// 鼠标上报模式（1000/1002/1003 互斥，最后置位者生效）。
    mouse: Option<u16>,
    /// `?1006h` SGR 鼠标编码扩展。
    mouse_sgr: bool,
    /// `?2004h` bracketed paste。
    bracketed_paste: bool,
    scan: ScanState,
}

impl ModeTracker {
    pub(crate) fn new() -> Self {
        Self {
            alt_screen: None,
            cursor_hidden: false,
            app_cursor_keys: false,
            mouse: None,
            mouse_sgr: false,
            bracketed_paste: false,
            scan: ScanState::Ground,
        }
    }

    /// 增量喂输出字节（读泵路径调用；锁纪律：纯内存、无 I/O 无回调——与
    /// scrollback 锁同享受表锁例外前提）。
    pub(crate) fn feed(&mut self, chunk: &[u8]) {
        for &b in chunk {
            self.step(b);
        }
    }

    fn step(&mut self, b: u8) {
        match &mut self.scan {
            ScanState::Ground => {
                if b == 0x1b {
                    self.scan = ScanState::Esc;
                }
            }
            ScanState::Esc => {
                self.scan = match b {
                    b'[' => ScanState::Csi {
                        private: false,
                        params: Vec::new(),
                        cur: 0,
                        has_cur: false,
                    },
                    0x1b => ScanState::Esc,
                    _ => ScanState::Ground,
                };
            }
            ScanState::Csi {
                private,
                params,
                cur,
                has_cur,
            } => match b {
                b'?' if !*private && params.is_empty() && !*has_cur => *private = true,
                b'0'..=b'9' => {
                    *cur = cur.saturating_mul(10).saturating_add((b - b'0') as u16);
                    *has_cur = true;
                }
                b';' => {
                    if params.len() < 16 {
                        params.push(*cur);
                    }
                    *cur = 0;
                    *has_cur = false;
                }
                // 序列内再见 ESC：当前序列作废，从 Esc 重启（撕裂/损坏容错）。
                0x1b => self.scan = ScanState::Esc,
                b'h' | b'l' => {
                    if *private {
                        let set = b == b'h';
                        let mut all = std::mem::take(params);
                        if *has_cur && all.len() < 16 {
                            all.push(*cur);
                        }
                        for m in all {
                            self.apply(m, set);
                        }
                    }
                    self.scan = ScanState::Ground;
                }
                // 其余 CSI 终结符（0x40-0x7E）或意外字节：放行回 Ground。
                _ if (0x40..=0x7e).contains(&b) => self.scan = ScanState::Ground,
                // 中间字节（0x20-0x2F）等：继续等待终结符。
                _ => {}
            },
        }
    }

    fn apply(&mut self, mode: u16, set: bool) {
        match mode {
            1049 | 1047 | 47 => self.alt_screen = if set { Some(mode) } else { None },
            25 => self.cursor_hidden = !set,
            1 => self.app_cursor_keys = set,
            1000 | 1002 | 1003 => self.mouse = if set { Some(mode) } else { None },
            1006 => self.mouse_sgr = set,
            2004 => self.bracketed_paste = set,
            _ => {}
        }
    }

    /// 合成 attach 重放前导：把当前非默认的模式位重建为 DECSET/DECRST 序列。
    /// 顺序约束：alt-screen 必须最先（`?1049h` 自带清屏，后续 ring 重绘覆盖其上）。
    /// 全默认态返回空（普通 shell 历史重放零开销）。
    pub(crate) fn preamble(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(m) = self.alt_screen {
            out.extend_from_slice(format!("\x1b[?{m}h").as_bytes());
        }
        if self.cursor_hidden {
            out.extend_from_slice(b"\x1b[?25l");
        }
        if self.app_cursor_keys {
            out.extend_from_slice(b"\x1b[?1h");
        }
        if let Some(m) = self.mouse {
            out.extend_from_slice(format!("\x1b[?{m}h").as_bytes());
        }
        if self.mouse_sgr {
            out.extend_from_slice(b"\x1b[?1006h");
        }
        if self.bracketed_paste {
            out.extend_from_slice(b"\x1b[?2004h");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(chunks: &[&[u8]]) -> ModeTracker {
        let mut t = ModeTracker::new();
        for c in chunks {
            t.feed(c);
        }
        t
    }

    #[test]
    fn default_state_has_empty_preamble() {
        assert!(ModeTracker::new().preamble().is_empty());
    }

    #[test]
    fn tracks_alt_screen_enter_and_exit() {
        let t = fed(&[b"\x1b[?1049h\x1b[2J\x1b[Hdraw"]);
        assert_eq!(t.preamble(), b"\x1b[?1049h".to_vec());
        let t = fed(&[b"\x1b[?1049hdraw\x1b[?1049l"]);
        assert!(t.preamble().is_empty(), "进出配平后应回默认态");
    }

    #[test]
    fn tracks_cursor_hidden_and_mouse_and_paste() {
        let t = fed(&[b"\x1b[?1049h\x1b[?25l\x1b[?1002h\x1b[?1006h\x1b[?2004h"]);
        let p = String::from_utf8(t.preamble()).unwrap();
        assert_eq!(p, "\x1b[?1049h\x1b[?25l\x1b[?1002h\x1b[?1006h\x1b[?2004h");
    }

    #[test]
    fn sequence_torn_across_chunks_is_reassembled() {
        // ?1049h 被切成三段（读泵任意边界）。
        let t = fed(&[b"\x1b[?10", b"49", b"h"]);
        assert_eq!(t.preamble(), b"\x1b[?1049h".to_vec());
        // h 单独一个 chunk。
        let t = fed(&[b"\x1b[?25l\x1b[?1049", b"h"]);
        let p = t.preamble();
        assert!(p.starts_with(b"\x1b[?1049h") && p.ends_with(b"\x1b[?25l"));
    }

    #[test]
    fn multi_param_decset_applies_each() {
        let t = fed(&[b"\x1b[?1049;25h"]); // 同序列置两模式：alt + 光标显示
        assert_eq!(t.preamble(), b"\x1b[?1049h".to_vec(), "25h=显示=默认，不进前导");
        let t = fed(&[b"\x1b[?1000;1006h"]);
        assert_eq!(t.preamble(), b"\x1b[?1000h\x1b[?1006h".to_vec());
    }

    #[test]
    fn non_private_csi_and_text_are_ignored() {
        // SGR 着色 / 光标定位 / 含 "?25l" 的纯文本都不影响状态。
        let t = fed(&[b"\x1b[31mred\x1b[0m\x1b[5;1H literal ?25l text \x1b[K"]);
        assert!(t.preamble().is_empty());
    }

    #[test]
    fn esc_inside_csi_restarts_cleanly() {
        // 损坏序列：CSI 中途撞 ESC——作废当前序列，新序列正常生效。
        let t = fed(&[b"\x1b[?10\x1b[?25lrest"]);
        let p = t.preamble();
        assert_eq!(p, b"\x1b[?25l".to_vec(), "前一半作废，?25l 生效");
    }

    #[test]
    fn mouse_last_set_wins_and_reset_clears() {
        let t = fed(&[b"\x1b[?1000h\x1b[?1002h"]);
        assert_eq!(t.preamble(), b"\x1b[?1002h".to_vec());
        let t = fed(&[b"\x1b[?1002h\x1b[?1002l"]);
        assert!(t.preamble().is_empty());
    }

    #[test]
    fn app_cursor_keys_tracked() {
        let t = fed(&[b"\x1b[?1h"]);
        assert_eq!(t.preamble(), b"\x1b[?1h".to_vec());
    }
}
