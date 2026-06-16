//! 行索引环形 scrollback（conmux API 契约 §5）。
//!
//! 在字节环 [`OutputBuffer`] 之上维护 `abs_line → abs_byte_offset` 索引，
//! 支持按**绝对物理行号**读取历史输出——这是后端生成 jump-back 落点
//! （`TerminalRange{coord_space=BackendAbs}`）的地基。
//!
//! ## 行号语义（冻结，总契约 §4.2）
//! `abs_line` = 自 pane 创建起的**写入侧物理行**（按 `\n` 计），单调递增、
//! 环覆盖后旧行不可读但**行号不复用**。它**不等于** xterm 视口行——坐标系
//! 消歧由 conflux 侧 `TerminalRange.coord_space` 承担（不在本模块）。
//!
//! 行 `i` 的内容定义为 `abs_byte[offset(i), offset(i+1))`（**含末尾 `\n`**）；
//! 最后一行（未遇 `\n`，未完成）为 `abs_byte[offset(last), total)`。
//!
//! 可见性：`pub(crate)`——对外只经 `CaptureResult` / `PaneState.scrollback`
//! 暴露语义化结果，不暴露缓冲本体。

// V0 增量：scrollback 在被 PaneHost / capture 接线前，pub(crate) 项暂为 dead；
// 后续增量接线后应移除本 allow。
#![allow(dead_code)]

use std::collections::VecDeque;

/// 默认 scrollback 容量：1 MB（总契约 D5，容量为 `new` 参数可调）。
pub(crate) const DEFAULT_BUFFER_CAPACITY: usize = 1_048_576;

// ===== 环形字节缓冲（retrofit 自 conflux `pty/buffer.rs`，已验证的环绕逻辑）=====

/// 固定容量环形缓冲：超出容量后丢弃最旧字节。`total_written` 单调累计
/// （含已被覆盖部分），用于换算绝对字节偏移。
pub(crate) struct OutputBuffer {
    data: Vec<u8>,
    capacity: usize,
    write_pos: usize,
    total_written: u64,
}

impl OutputBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = if capacity == 0 { 1 } else { capacity };
        Self {
            data: vec![0u8; capacity],
            capacity,
            write_pos: 0,
            total_written: 0,
        }
    }

    pub(crate) fn write(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        // 数据比整个缓冲还大时，只保留最后 capacity 字节。
        let src = if data.len() > self.capacity {
            &data[data.len() - self.capacity..]
        } else {
            data
        };
        let src_len = src.len();
        let remaining = self.capacity - self.write_pos;
        if src_len <= remaining {
            self.data[self.write_pos..self.write_pos + src_len].copy_from_slice(src);
        } else {
            self.data[self.write_pos..self.write_pos + remaining]
                .copy_from_slice(&src[..remaining]);
            let overflow = src_len - remaining;
            self.data[..overflow].copy_from_slice(&src[remaining..]);
        }
        self.write_pos = (self.write_pos + src_len) % self.capacity;
        // 记录原始完整长度（即便部分被丢弃），保证绝对偏移单调。
        self.total_written += data.len() as u64;
    }

    /// 读取最后 n 字节（按时间顺序，最旧在前）。n 超过有效长度则返回全部有效数据。
    pub(crate) fn read_last(&self, n: usize) -> Vec<u8> {
        let valid_len = self.len();
        if n == 0 || valid_len == 0 {
            return Vec::new();
        }
        let n = n.min(valid_len);
        if self.total_written <= self.capacity as u64 {
            self.data[self.write_pos - n..self.write_pos].to_vec()
        } else if n <= self.write_pos {
            self.data[self.write_pos - n..self.write_pos].to_vec()
        } else {
            let from_end = n - self.write_pos;
            let mut result = Vec::with_capacity(n);
            result.extend_from_slice(&self.data[self.capacity - from_end..]);
            result.extend_from_slice(&self.data[..self.write_pos]);
            result
        }
    }

    /// 当前有效数据长度（字节）。
    pub(crate) fn len(&self) -> usize {
        if self.total_written <= self.capacity as u64 {
            self.total_written as usize
        } else {
            self.capacity
        }
    }

    /// 累计写入总字节数（含已被覆盖部分）——绝对字节坐标系的高水位。
    pub(crate) fn total_written(&self) -> u64 {
        self.total_written
    }
}

// ===== 行索引缓冲 =====

/// 在字节环上叠加行号索引。
pub(crate) struct LineIndexedBuffer {
    bytes: OutputBuffer,
    /// `(abs_line, abs_byte_offset)`——每行起始的绝对字节偏移。单调递增；
    /// 环覆盖后，起始已不可读（`< oldest_readable`）的行被 `prune` 淘汰。
    line_starts: VecDeque<(u64, u64)>,
    /// 下一个将被分配的行号 = 当前正在写入的（未完成）行号。
    next_abs_line: u64,
    /// **PaneOutput 序号**（attach 无缝拼接锚，M2 设计 D-6）。从 0 起，`append_and_seq`
    /// 每次 +1（首块 → 1，per-pane 严格单调）。**与字节追加在同一锁域内原子绑定**：
    /// 任何 emit 的 seq=S 对应的 ring 状态必含该块字节；attach 快照在锁内同时读
    /// `read_all_bytes()` 与 `seq()`，保证 (历史, last_seq) 原子对应——客户端喂历史后
    /// 只喂 `seq > last_seq` 的 live 帧即无丢帧无重帧。
    seq: u64,
}

impl LineIndexedBuffer {
    pub(crate) fn new(capacity_bytes: usize) -> Self {
        let mut line_starts = VecDeque::new();
        line_starts.push_back((0, 0)); // 行 0 从绝对字节 0 开始
        Self {
            bytes: OutputBuffer::new(capacity_bytes),
            line_starts,
            next_abs_line: 0,
            seq: 0,
        }
    }

    /// 追加一段输出：写入字节环并扫描 `\n` 维护行索引（读线程内联调用）。
    pub(crate) fn append(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        let base = self.bytes.total_written(); // 本 chunk 首字节的绝对偏移
        self.bytes.write(chunk);
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'\n' {
                // `\n` 结束当前行，开启下一行（起始 = 该 \n 的下一字节）。
                self.next_abs_line += 1;
                let line_start_abs = base + i as u64 + 1;
                self.line_starts.push_back((self.next_abs_line, line_start_abs));
            }
        }
        self.prune();
    }

    /// 追加一段输出并返回**与之原子绑定**的新 PaneOutput 序号（D-6）。读泵在
    /// scrollback 锁内调用本方法取 seq，锁外 emit `PaneOutput{seq}`——保证 seq=S 的帧
    /// 对应的 ring 状态必已含本块字节（attach 快照锁内同读 `read_all_bytes`+`seq` 即原子）。
    pub(crate) fn append_and_seq(&mut self, chunk: &[u8]) -> u64 {
        self.append(chunk);
        self.seq += 1;
        self.seq
    }

    /// 当前 PaneOutput 序号高水位（attach 快照 last_seq）。
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    /// 当前（未完成）行的行号——写入侧高水位。
    pub(crate) fn current_line(&self) -> u64 {
        self.next_abs_line
    }

    /// 当前 ring 内有效字节数（capture / ScrollbackInfo.total_bytes 用）。
    pub(crate) fn total_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// 读取 ring 内全部有效字节（capture `All`）= read_last(len)。
    pub(crate) fn read_all_bytes(&self) -> Vec<u8> {
        self.bytes.read_last(self.bytes.len())
    }

    /// 读取最后 n 字节（capture `LastBytes`）。
    pub(crate) fn read_last_bytes(&self, n: usize) -> Vec<u8> {
        self.bytes.read_last(n)
    }

    /// ring 内仍可完整读取的行窗 `(first, last)`，`last` = 当前行。
    /// 无任何完整可读行时返回 `(next_abs_line, next_abs_line)`。
    pub(crate) fn line_range_available(&self) -> (u64, u64) {
        let first = self
            .line_starts
            .front()
            .map(|&(l, _)| l)
            .unwrap_or(self.next_abs_line);
        (first, self.next_abs_line)
    }

    /// 读取 `[start, end)` 行的字节。任一行起始已被环覆盖、或越界则返回 `None`
    /// （**不静默返回部分数据**）。`start >= end` 返回空。
    pub(crate) fn read_lines(&self, start: u64, end: u64) -> Option<Vec<u8>> {
        if start >= end {
            return Some(Vec::new());
        }
        let (avail_first, _) = self.line_range_available();
        if start < avail_first || end > self.next_abs_line + 1 {
            return None;
        }
        let start_abs = self.line_abs_offset(start)?;
        // 行 end 的起始；end 行尚未开始（end > 当前行）则读到高水位。
        let end_abs = if end > self.next_abs_line {
            self.bytes.total_written()
        } else {
            self.line_abs_offset(end)?
        };
        self.read_abs_range(start_abs, end_abs)
    }

    fn oldest_readable_abs(&self) -> u64 {
        self.bytes.total_written().saturating_sub(self.bytes.len() as u64)
    }

    /// 淘汰起始已被环覆盖的行（起始 `abs_byte < oldest_readable`）。
    fn prune(&mut self) {
        let oldest = self.oldest_readable_abs();
        while let Some(&(_, start)) = self.line_starts.front() {
            if start < oldest {
                self.line_starts.pop_front();
            } else {
                break;
            }
        }
    }

    fn line_abs_offset(&self, line: u64) -> Option<u64> {
        self.line_starts
            .iter()
            .find(|&&(l, _)| l == line)
            .map(|&(_, off)| off)
    }

    /// 读取绝对字节区间 `[a, b)`；区间任一端越出可读窗口返回 `None`。
    fn read_abs_range(&self, a: u64, b: u64) -> Option<Vec<u8>> {
        if a >= b {
            return Some(Vec::new());
        }
        if a < self.oldest_readable_abs() || b > self.bytes.total_written() {
            return None;
        }
        let from_end = (self.bytes.total_written() - a) as usize;
        let mut data = self.bytes.read_last(from_end);
        data.truncate((b - a) as usize);
        Some(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_starts_at_line_zero() {
        let buf = LineIndexedBuffer::new(DEFAULT_BUFFER_CAPACITY);
        assert_eq!(buf.current_line(), 0);
        assert_eq!(buf.line_range_available(), (0, 0));
        // 空 buffer：行 0 尚无内容，读回空
        assert_eq!(buf.read_lines(0, 1).as_deref(), Some(&b""[..]));
    }

    #[test]
    fn single_unfinished_line_reads_back() {
        let mut buf = LineIndexedBuffer::new(1024);
        buf.append(b"abc");
        assert_eq!(buf.current_line(), 0); // 未遇 \n，仍在行 0
        assert_eq!(buf.read_lines(0, 1).as_deref(), Some(&b"abc"[..]));
    }

    #[test]
    fn lines_include_trailing_newline() {
        let mut buf = LineIndexedBuffer::new(1024);
        buf.append(b"line0\nline1\n");
        assert_eq!(buf.current_line(), 2); // 写了 2 个 \n → 行 0、1 完成，行 2 为空
        assert_eq!(buf.read_lines(0, 1).as_deref(), Some(&b"line0\n"[..]));
        assert_eq!(buf.read_lines(1, 2).as_deref(), Some(&b"line1\n"[..]));
        assert_eq!(buf.read_lines(0, 2).as_deref(), Some(&b"line0\nline1\n"[..]));
    }

    #[test]
    fn unfinished_last_line_reads_to_high_watermark() {
        let mut buf = LineIndexedBuffer::new(1024);
        buf.append(b"done\npartial");
        assert_eq!(buf.current_line(), 1);
        assert_eq!(buf.read_lines(1, 2).as_deref(), Some(&b"partial"[..]));
    }

    #[test]
    fn append_across_chunks_keeps_abs_line_monotonic() {
        let mut buf = LineIndexedBuffer::new(1024);
        buf.append(b"a\n");
        buf.append(b"b\n");
        buf.append(b"c");
        assert_eq!(buf.current_line(), 2);
        assert_eq!(buf.read_lines(0, 1).as_deref(), Some(&b"a\n"[..]));
        assert_eq!(buf.read_lines(2, 3).as_deref(), Some(&b"c"[..]));
    }

    #[test]
    fn ring_overflow_drops_oldest_lines_but_not_line_numbers() {
        // 容量 8：写入 "aa\nbb\ncc\n"（9 字节）→ 丢最旧 1 字节，行 0 起始被覆盖。
        let mut buf = LineIndexedBuffer::new(8);
        buf.append(b"aa\nbb\ncc\n");
        assert_eq!(buf.current_line(), 3); // 行号不复用，仍是 3
        let (first, last) = buf.line_range_available();
        assert!(first >= 1, "行 0 起始被覆盖，可读窗口应前移，first={first}");
        assert_eq!(last, 3);
        // 行 0 不完整可读 → None（不静默返回部分）
        assert_eq!(buf.read_lines(0, 1), None);
        // 行 1 仍完整可读
        assert_eq!(buf.read_lines(1, 2).as_deref(), Some(&b"bb\n"[..]));
    }

    #[test]
    fn read_beyond_available_returns_none() {
        let mut buf = LineIndexedBuffer::new(1024);
        buf.append(b"x\ny\n");
        assert_eq!(buf.read_lines(0, 99), None); // end 越界
        assert_eq!(buf.read_lines(2, 3).as_deref(), Some(&b""[..])); // 行 2 空（未开始内容）
    }

    #[test]
    fn oversized_single_line_has_no_complete_readable_line() {
        // 容量 4，单行 6 字节无 \n：当前行起始被覆盖，无完整可读行。
        let mut buf = LineIndexedBuffer::new(4);
        buf.append(b"aaaaaa");
        assert_eq!(buf.current_line(), 0);
        // 行 0 起始（abs 0）已被覆盖 → 读不到完整行
        assert_eq!(buf.read_lines(0, 1), None);
    }

    #[test]
    fn append_and_seq_is_monotonic_and_atomic_with_bytes() {
        // D-6：seq 从 1 起严格单调；每个 seq 对应的 ring 已含该块字节。
        let mut buf = LineIndexedBuffer::new(1024);
        assert_eq!(buf.seq(), 0, "初始 seq=0（尚无 emit）");
        let s1 = buf.append_and_seq(b"first\n");
        assert_eq!(s1, 1);
        assert_eq!(buf.seq(), 1);
        // seq=1 时快照（read_all_bytes + seq）必含 first 块。
        assert!(buf.read_all_bytes().windows(5).any(|w| w == b"first"));
        let s2 = buf.append_and_seq(b"second\n");
        assert_eq!(s2, 2);
        assert_eq!(buf.seq(), 2);
        // 快照 seq=2 含 first+second（原子对应）。
        let snap = buf.read_all_bytes();
        assert!(snap.windows(6).any(|w| w == b"second"));
        assert!(snap.windows(5).any(|w| w == b"first"));
    }

    #[test]
    fn output_buffer_ring_wrap_reads_correctly() {
        let mut b = OutputBuffer::new(4);
        b.write(b"abcdef"); // 只留最后 4: "cdef"
        assert_eq!(b.total_written(), 6);
        assert_eq!(b.len(), 4);
        assert_eq!(b.read_last(4), b"cdef");
        assert_eq!(b.read_last(2), b"ef");
    }
}
