//! 长度前缀帧编码（M2 设计 D-4）：`u32 LE 长度 + JSON(WireFrame)`。
//!
//! 跨平台纯逻辑（操作任意 `Read`/`Write`）——命名管道字节流、内存缓冲、测试 mock 通用。
//! JSON 保留 serde 形状即契约（F-1）与可调试性；长度前缀对二进制内容免疫
//! （不依赖换行分帧，base64 后的 PaneOutput 含任意字节）。
//!
//! ## 不变量
//! - **单帧上限 [`MAX_FRAME_BYTES`] = 4 MiB**（capture 1 MiB ring base64 后 ~1.4 MiB 留余量）。
//!   超限在**两端都拒**：写端序列化超限 → `Oversize`（不发半帧）；读端**先看长度字段**，
//!   超限立即拒、**绝不按恶意长度预分配**（DoS 面收口）。
//! - **EOF 区分**：长度前缀处读到 0 字节 = 对端在帧边界正常关闭（`Eof`）；读到部分字节 =
//!   帧中途截断（`Io(UnexpectedEof)`，异常）。消费方据此区分优雅断开与故障。
//! - 帧方向约束（H-2）不在本层强制——本层只做编解码；方向由 daemon/client 解析后判定
//!   （见 `protocol::WireFrame` 文档 + daemon dispatcher）。

use std::io::{self, Read, Write};

use crate::protocol::WireFrame;

/// 单帧字节上限（4 MiB）。声明长度或序列化结果超限即协议错误。
pub const MAX_FRAME_BYTES: u32 = 4 * 1024 * 1024;

/// 帧编解码错误。
#[derive(Debug)]
pub enum WireError {
    /// 底层 I/O 错误（含帧中途截断 = `UnexpectedEof`）。
    Io(io::Error),
    /// 对端在帧边界正常关闭连接（长度前缀处读到 0 字节）。非故障。
    Eof,
    /// 声明长度（读）或序列化结果（写）超过 [`MAX_FRAME_BYTES`]。
    Oversize(u32),
    /// JSON 编解码失败（含未知变体 / `deny_unknown_fields` 拒收）。
    Json(serde_json::Error),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Io(e) => write!(f, "wire I/O 错误: {e}"),
            WireError::Eof => write!(f, "对端在帧边界关闭连接"),
            WireError::Oversize(n) => write!(f, "帧超限: {n} > {MAX_FRAME_BYTES} 字节"),
            WireError::Json(e) => write!(f, "wire JSON 错误: {e}"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<io::Error> for WireError {
    fn from(e: io::Error) -> Self {
        WireError::Io(e)
    }
}

impl From<serde_json::Error> for WireError {
    fn from(e: serde_json::Error) -> Self {
        WireError::Json(e)
    }
}

/// 写一帧：`u32 LE 长度 + JSON`，随后 flush。序列化超限 → `Oversize`（不发半帧）。
pub fn write_frame<W: Write>(w: &mut W, frame: &WireFrame) -> Result<(), WireError> {
    let json = serde_json::to_vec(frame)?;
    if json.len() > MAX_FRAME_BYTES as usize {
        // u32 截断不影响判定（已知超限）；取饱和值仅用于错误展示。
        return Err(WireError::Oversize(
            json.len().min(u32::MAX as usize) as u32,
        ));
    }
    let len = json.len() as u32;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&json)?;
    w.flush()?;
    Ok(())
}

/// 读一帧。长度前缀处 0 字节 → `Eof`；声明长度超限 → `Oversize`（**不预分配**）；
/// 长度声明后字节不足 → `Io(UnexpectedEof)`。
pub fn read_frame<R: Read>(r: &mut R) -> Result<WireFrame, WireError> {
    let mut len_buf = [0u8; 4];
    read_len_prefix(r, &mut len_buf)?;
    let len = u32::from_le_bytes(len_buf);
    // 先按长度字段拒超限——绝不据恶意长度 `vec![0; len]` 预分配。
    if len > MAX_FRAME_BYTES {
        return Err(WireError::Oversize(len));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?; // 不足 = UnexpectedEof（帧截断，异常）
    let frame = serde_json::from_slice(&buf)?;
    Ok(frame)
}

/// 读 4 字节长度前缀，区分「帧边界正常关闭」与「中途截断」。
/// 首字节起读到 0 字节 → `Eof`；读到部分（1..4）→ `UnexpectedEof`。
fn read_len_prefix<R: Read>(r: &mut R, buf: &mut [u8; 4]) -> Result<(), WireError> {
    let mut filled = 0;
    while filled < 4 {
        match r.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Err(WireError::Eof); // 帧边界正常关闭
                }
                return Err(WireError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "长度前缀中途截断",
                )));
            }
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(WireError::Io(e)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{MuxOp, MuxReply, MuxRequest, PROTOCOL_VERSION};
    use crate::types::PaneId;

    fn sample_frames() -> Vec<WireFrame> {
        vec![
            WireFrame::Hello {
                protocol_version: PROTOCOL_VERSION,
                client_kind: "conmux-cli".into(),
            },
            WireFrame::HelloAck {
                protocol_version: PROTOCOL_VERSION,
                daemon_version: "0.1.0".into(),
            },
            WireFrame::Request(MuxRequest {
                correlation_id: 9,
                op: MuxOp::ListPanes,
            }),
            WireFrame::Reply(MuxReply::Ok {
                correlation_id: 9,
                payload: crate::protocol::MuxPayload::Panes(vec![]),
            }),
            WireFrame::Notify(crate::MuxNotify::PaneOutput {
                pane_id: PaneId("p1".into()),
                seq: 3,
                data: vec![0x00, 0x1b, 0x5b, 0xff], // 非 UTF-8，验证 base64 + 二进制免疫
            }),
        ]
    }

    /// 单帧写后读回，逐变体无损往返。
    #[test]
    fn single_frame_round_trips() {
        for frame in sample_frames() {
            let mut buf = Vec::new();
            write_frame(&mut buf, &frame).unwrap();
            let mut cursor = io::Cursor::new(buf);
            let back = read_frame(&mut cursor).unwrap();
            assert_eq!(frame, back);
        }
    }

    /// 多帧背靠背写入同一缓冲，顺序读回——验证长度前缀正确分帧。
    #[test]
    fn multiple_frames_back_to_back() {
        let frames = sample_frames();
        let mut buf = Vec::new();
        for f in &frames {
            write_frame(&mut buf, f).unwrap();
        }
        let mut cursor = io::Cursor::new(buf);
        for f in &frames {
            let back = read_frame(&mut cursor).unwrap();
            assert_eq!(*f, back);
        }
        // 末尾再读 = 帧边界正常关闭。
        assert!(matches!(read_frame(&mut cursor), Err(WireError::Eof)));
    }

    /// 空读端 = 帧边界 EOF（优雅关闭，非故障）。
    #[test]
    fn empty_reader_is_clean_eof() {
        let mut cursor = io::Cursor::new(Vec::new());
        assert!(matches!(read_frame(&mut cursor), Err(WireError::Eof)));
    }

    /// 长度前缀读了一半就断 = UnexpectedEof（截断，区别于优雅 EOF）。
    #[test]
    fn truncated_len_prefix_is_unexpected_eof() {
        let mut cursor = io::Cursor::new(vec![0x10, 0x00]); // 仅 2/4 字节
        match read_frame(&mut cursor) {
            Err(WireError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
            other => panic!("应为 UnexpectedEof，实际: {other:?}"),
        }
    }

    /// 长度声明合法但 body 不足 = UnexpectedEof（帧截断）。
    #[test]
    fn truncated_body_is_unexpected_eof() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&100u32.to_le_bytes()); // 声明 100 字节
        buf.extend_from_slice(b"only a few"); // 实际不足
        let mut cursor = io::Cursor::new(buf);
        match read_frame(&mut cursor) {
            Err(WireError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
            other => panic!("应为 UnexpectedEof，实际: {other:?}"),
        }
    }

    /// **DoS 面**：恶意超大长度字段被立即拒，**不预分配**。
    #[test]
    fn oversize_length_field_rejected_without_allocating() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_le_bytes());
        // 故意不附 body——若实现先 vec![0; len] 预分配再 read_exact，会在分配/读取处卡住或 OOM；
        // 正确实现先按长度字段拒，根本不读 body。
        let mut cursor = io::Cursor::new(buf);
        match read_frame(&mut cursor) {
            Err(WireError::Oversize(n)) => assert_eq!(n, MAX_FRAME_BYTES + 1),
            other => panic!("应为 Oversize，实际: {other:?}"),
        }
    }

    /// 损坏 JSON body（长度合法）= Json 错误，不 panic。
    #[test]
    fn corrupt_json_body_is_json_error() {
        let bad = b"not json at all!";
        let mut buf = Vec::new();
        buf.extend_from_slice(&(bad.len() as u32).to_le_bytes());
        buf.extend_from_slice(bad);
        let mut cursor = io::Cursor::new(buf);
        assert!(matches!(read_frame(&mut cursor), Err(WireError::Json(_))));
    }
}
