//! Windows ConPTY 后端（cutover 2b-2 / 契约 §3 / D2）。
//!
//! 用 portable-pty 0.9 实现 `PaneBackend`/`PaneSession`，retrofit 自现状
//! `pty/manager.rs::spawn_inner`（0.8）。**0.9 关键差异（spike 实证）**：无条件设
//! `INHERIT_CURSOR` → ConPTY 启动即发 DSR `ESC[6n` 并阻塞等光标位置应答；故读线程
//! 必须内联应答 `ESC[1;1R`（[`pump_reader_with_dsr`]），否则连 `cmd /c echo` 都挂死。
//!
//! 仅 `cfg(windows)` 编译。
//!
//! **写路径与 MF-1**：注入（agent 输入）唯一经 `PaneSession::write_all`（PaneHost
//! inject_stdin 钩子链）。DSR 应答是 conmux 对终端协议查询的**机制层回复、非 agent 输入**，
//! 由读线程经 [`writer_arc`](WindowsPaneSession::writer_arc) 直写——它不是调用方可达的
//! 第二写路径（reader 线程 conmux 内部持有），故不违反 MF-1（MF-1 防的是调用方绕过注入审计）。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::pane::{CommandSpec, PaneBackend, PaneSession};
use crate::types::PaneSize;
use crate::ConmuxError;

// DSR 常量 / pump_reader_with_dsr / protocol_writer 在 2b-3 PaneHost 读线程接线前，
// lib build 暂为 dead（仅 2b-2 集成测试用）。接线后移除这些 allow。
/// DSR 光标位置查询 / 应答（ConPTY INHERIT_CURSOR 启动时必发查询）。
#[allow(dead_code)]
const DSR_QUERY: &[u8] = b"\x1b[6n";
#[allow(dead_code)]
const DSR_REPLY: &[u8] = b"\x1b[1;1R";

fn to_pty_size(size: PaneSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Windows ConPTY 后端工厂。
pub struct WindowsPaneBackend;

impl PaneBackend for WindowsPaneBackend {
    fn open(&self, size: PaneSize) -> Result<Box<dyn PaneSession>, ConmuxError> {
        let pair = native_pty_system()
            .openpty(to_pty_size(size))
            .map_err(|e| ConmuxError::PtyError {
                message: format!("openpty 失败: {e}"),
            })?;
        Ok(Box::new(WindowsPaneSession {
            master: pair.master,
            slave: Some(pair.slave),
            child: None,
            writer: None,
            reader_taken: false,
        }))
    }
}

/// 单个 ConPTY 会话。`master` 持有用于 resize/reader/writer；`slave` spawn 后即 drop
/// （对齐现状 manager）；writer 置于 `Arc<Mutex>` 供 inject 写与读线程 DSR 应答共享。
pub struct WindowsPaneSession {
    master: Box<dyn portable_pty::MasterPty + Send>,
    slave: Option<Box<dyn portable_pty::SlavePty + Send>>,
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    reader_taken: bool,
}

impl PaneSession for WindowsPaneSession {
    /// 非阻塞查询子进程是否已退出（D-2a poll_exit / PaneExited 退出码；kill 成败不信此
    /// 返回值，见 spike #5）。
    fn try_exit_code(&mut self) -> Option<i32> {
        match self.child.as_mut()?.try_wait() {
            Ok(Some(status)) => Some(status.exit_code() as i32),
            _ => None,
        }
    }

    fn spawn(&mut self, cmd: &CommandSpec) -> Result<u32, ConmuxError> {
        let slave = self.slave.as_ref().ok_or_else(|| ConmuxError::SpawnFailed {
            message: "session 已 spawn 或 slave 缺失".into(),
        })?;

        let mut builder = CommandBuilder::new(&cmd.program);
        for arg in &cmd.args {
            builder.arg(arg);
        }
        if let Some(cwd) = &cmd.cwd {
            builder.cwd(cwd);
        }
        for (k, v) in &cmd.env {
            builder.env(k, v);
        }

        let child = slave
            .spawn_command(builder)
            .map_err(|e| ConmuxError::SpawnFailed {
                message: format!("spawn_command 失败: {e}"),
            })?;
        let pid = child.process_id().unwrap_or(0);

        let writer = self.master.take_writer().map_err(|e| ConmuxError::PtyError {
            message: format!("take_writer 失败: {e}"),
        })?;

        // spawn 后释放 slave（对齐现状 manager；master 仍持有用于 resize/reader/writer）。
        self.slave = None;
        self.child = Some(child);
        self.writer = Some(Arc::new(Mutex::new(writer)));
        Ok(pid)
    }

    fn take_reader(&mut self) -> Result<Box<dyn Read + Send>, ConmuxError> {
        if self.reader_taken {
            return Err(ConmuxError::PtyError {
                message: "reader 已被移交（一次性，MF-1）".into(),
            });
        }
        let reader = self
            .master
            .try_clone_reader()
            .map_err(|e| ConmuxError::PtyError {
                message: format!("try_clone_reader 失败: {e}"),
            })?;
        self.reader_taken = true;
        Ok(reader)
    }

    fn resize(&self, size: PaneSize) -> Result<(), ConmuxError> {
        self.master
            .resize(to_pty_size(size))
            .map_err(|e| ConmuxError::PtyError {
                message: format!("resize 失败: {e}"),
            })
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), ConmuxError> {
        let writer = self.writer.as_ref().ok_or_else(|| ConmuxError::PtyError {
            message: "writer 未就绪（spawn 之前不可写）".into(),
        })?;
        let mut guard = writer.lock().expect("writer 锁未中毒");
        guard.write_all(data).map_err(|e| ConmuxError::PtyError {
            message: format!("stdin 写入失败: {e}"),
        })?;
        guard.flush().map_err(|e| ConmuxError::PtyError {
            message: format!("stdin flush 失败: {e}"),
        })
    }

    #[allow(dead_code)] // 2b-3 PaneHost 读线程会用（DSR 应答）。
    fn protocol_writer(&self) -> Option<Arc<Mutex<Box<dyn Write + Send>>>> {
        self.writer.clone()
    }

    /// best-effort 终结子进程（红队 MF-A：assign 失败分支无监管进程必须主动杀）。
    /// portable-pty 0.9 kill 返回值不可信（spike #5），故不看结果；kill 后 best-effort
    /// wait 回收。drop session 关 ConPTY 两端是补充清理，但孤儿孙进程靠它不可靠。
    fn kill_best_effort(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 读线程主循环：持续读 `reader`，内联应答 DSR `ESC[6n`（经 `writer` 直写——机制层
/// 协议回复，不过注入钩子），每块原始输出回调 `sink`。reader EOF / 错误即返回。
///
/// 跨 chunk 检测 DSR：保留上一块尾部 3 字节与本块拼接搜索（`ESC[6n` 4 字节，防截断）。
/// 2b-2 集成测试与 2b-3 PaneHost 读线程共用本函数。
#[allow(dead_code)] // 2b-3 PaneHost 读线程接线前 lib build 暂为 dead。
pub(crate) fn pump_reader_with_dsr<F>(
    mut reader: Box<dyn Read + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    mut sink: F,
) where
    F: FnMut(&[u8]),
{
    let mut carry: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break, // EOF（进程退出 + master drop）或管道断
            Ok(n) => {
                let chunk = &buf[..n];
                // DSR 检测：carry + chunk 拼接搜索 ESC[6n。
                let mut scan = Vec::with_capacity(carry.len() + n);
                scan.extend_from_slice(&carry);
                scan.extend_from_slice(chunk);
                let dsr_count = scan.windows(DSR_QUERY.len()).filter(|w| *w == DSR_QUERY).count();
                if dsr_count > 0 {
                    if let Ok(mut guard) = writer.lock() {
                        for _ in 0..dsr_count {
                            let _ = guard.write_all(DSR_REPLY);
                        }
                        let _ = guard.flush();
                    }
                }
                carry = scan[scan.len().saturating_sub(DSR_QUERY.len() - 1)..].to_vec();
                sink(chunk);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn windows_session_spawn_read_roundtrip_with_dsr() {
        let backend = WindowsPaneBackend;
        let mut session = backend
            .open(PaneSize { rows: 24, cols: 80 })
            .expect("open 应成功");
        let cmd = CommandSpec {
            program: "cmd.exe".into(),
            args: vec!["/c".into(), "echo conmux-2b2-marker".into()],
            cwd: None,
            env: vec![],
        };
        let pid = session.spawn(&cmd).expect("spawn 应成功");
        assert!(pid > 0, "应拿到真实 pid");

        let reader = session.take_reader().expect("take_reader 应成功");
        // 二次 take_reader 必败（MF-1 一次性）。
        assert!(session.take_reader().is_err());
        let writer = session.protocol_writer().expect("spawn 后 protocol_writer 就绪");

        let collected = Arc::new(Mutex::new(Vec::<u8>::new()));
        let c2 = Arc::clone(&collected);
        let handle = std::thread::spawn(move || {
            pump_reader_with_dsr(reader, writer, |chunk| {
                c2.lock().unwrap().extend_from_slice(chunk);
            });
        });

        // 给 echo 跑完 + DSR 应答 + 输出流动的时间，然后 drop session（关 master）→ reader EOF。
        std::thread::sleep(Duration::from_millis(1500));
        drop(session); // master drop → ClosePseudoConsole → reader EOF → pump 返回
        handle.join().expect("读线程不应 panic");

        let bytes = collected.lock().unwrap().clone();
        let out = String::from_utf8_lossy(&bytes);
        assert!(
            out.contains("conmux-2b2-marker"),
            "输出应含 echo 内容（DSR 已应答否则会挂死），实际:\n{out}"
        );
    }

    #[test]
    fn write_before_spawn_errors() {
        let backend = WindowsPaneBackend;
        let mut session = backend.open(PaneSize { rows: 24, cols: 80 }).unwrap();
        assert!(session.write_all(b"x").is_err(), "spawn 前不可写");
        assert!(session.protocol_writer().is_none());
    }

    #[test]
    fn resize_succeeds_on_open_session() {
        let backend = WindowsPaneBackend;
        let session = backend.open(PaneSize { rows: 24, cols: 80 }).unwrap();
        assert!(session.resize(PaneSize { rows: 40, cols: 120 }).is_ok());
    }
}
