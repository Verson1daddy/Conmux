//! D2 spike（2026-06-11）：portable-pty 0.9 + ConPTY 底座可行性验证。
//!
//! 目的（契约 D2 / V0-6 的前置去险，先于 PaneHost 实做）：
//! 1. portable-pty 0.8 → 0.9 升级后，在本机（Win11 26200）经 ConPTY spawn 真实进程、
//!    读输出、拿退出码的全链路是否成立；
//! 2. 验证读线程模型（ConPTY 无 async I/O，阻塞读 + EOF 退出）与 kill 行为；
//! 3. DSR `ESC[6n` 行为（wezterm#6783）。
//!
//! **Spike 实锤结论（2026-06-11 第一轮运行）**：portable-pty 0.9 无条件设
//! `PSUEDOCONSOLE_INHERIT_CURSOR`（psuedocon.rs:87），系统内置 ConPTY 启动时发
//! `ESC[6n` 并**阻塞到收到光标位置应答**——不应答则 `cmd /c echo` 都挂死（>60s）。
//! 因此本文件的读线程内置 DSR 自动应答（`ESC[1;1R`），这正是 PaneHost 读线程
//! 第一版就必须内联实现的机制（契约 D2 第 2 步，提前到 V0 生效）。
//!
//! 发现记录在 `.workbench/coordination/research/conpty-spike-2026-06-11.md`。
//! 这些测试 spawn 真实 cmd.exe——仅在 Windows 上有意义。

#![cfg(windows)]

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn default_size() -> PtySize {
    PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// DSR 光标位置查询（ConPTY INHERIT_CURSOR 启动时必发）。
const DSR_QUERY: &[u8] = b"\x1b[6n";
/// 应答：光标在 1,1。
const DSR_REPLY: &[u8] = b"\x1b[1;1R";

/// 读线程 + DSR 自动应答：扫描输出流，见 `ESC[6n` 即回写 `ESC[1;1R`。
/// 返回 (join handle → 全部输出, 是否观测到 DSR)。
/// 这是 PaneHost 读线程的最小机制雏形。
fn spawn_reader_with_dsr_answer(
    mut reader: Box<dyn Read + Send>,
    mut writer: Box<dyn Write + Send>,
) -> (thread::JoinHandle<Vec<u8>>, Arc<AtomicBool>) {
    let saw_dsr = Arc::new(AtomicBool::new(false));
    let saw_dsr_clone = saw_dsr.clone();
    let handle = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut all: Vec<u8> = Vec::new();
        let mut answered = false;
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break, // EOF：进程退出 + master drop 后到达
                Ok(n) => {
                    all.extend_from_slice(&buf[..n]);
                    // DSR 查询可能跨 chunk 拆分——对累计缓冲扫描（spike 级实现；
                    // PaneHost 正式版应在解析器里做增量状态机）。
                    if !answered && all.windows(DSR_QUERY.len()).any(|w| w == DSR_QUERY) {
                        saw_dsr_clone.store(true, Ordering::SeqCst);
                        let _ = writer.write_all(DSR_REPLY);
                        let _ = writer.flush();
                        answered = true;
                    }
                }
            }
        }
        all
    });
    (handle, saw_dsr)
}

/// 全链路 roundtrip：openpty → spawn `cmd /c echo` → 读输出（含 DSR 应答）→ 退出码 0。
/// 这是 PaneBackend 真实实现将依赖的最小机制闭环。
#[test]
fn spawn_cmd_echo_roundtrip_via_conpty() {
    let pty = native_pty_system()
        .openpty(default_size())
        .expect("openpty 失败：ConPTY 不可用？");

    let mut cmd = CommandBuilder::new("cmd.exe");
    cmd.args(["/c", "echo conmux-d2-spike"]);
    let mut child = pty.slave.spawn_command(cmd).expect("spawn cmd.exe 失败");

    let reader = pty.master.try_clone_reader().expect("clone reader 失败");
    let writer = pty.master.take_writer().expect("take writer 失败");
    let (read_thread, saw_dsr) = spawn_reader_with_dsr_answer(reader, writer);

    let status = child.wait().expect("wait 失败");
    assert!(status.success(), "cmd /c echo 应以 0 退出，实际: {status:?}");

    // 进程已退出；**slave 与 master 都要 drop** 读线程才拿得到 EOF——
    // portable-pty 的 master/slave 共享 Arc<Inner>（内含 HPCON），
    // ClosePseudoConsole 在最后一个 Arc drop 时才触发（spike 实锤，见报告发现 #5）。
    drop(pty.slave);
    drop(pty.master);
    let all = read_thread.join().expect("读线程 panic");

    let text = String::from_utf8_lossy(&all);
    assert!(
        text.contains("conmux-d2-spike"),
        "输出应含 echo 内容（含 VT 序列原文允许），实际:\n{text}"
    );
    eprintln!(
        "[spike] DSR ESC[6n observed: {} (bytes={})",
        saw_dsr.load(Ordering::SeqCst),
        all.len()
    );
}

/// kill 行为：长驻进程被 child.kill 终结，读线程随 EOF 收口、不悬挂。
/// 对应 PaneHost::kill 的最小语义（JobObject 整树监管是其上叠加，另测）。
///
/// **Spike 教训（实测挂死后修正）**：不能用 `cmd /c ping -t` 包装——kill 只杀
/// cmd.exe，孙进程 ping.exe 成为孤儿继续持有 ConPTY，`drop(master)` 触发的
/// `ClosePseudoConsole` 会阻塞到客户端全灭 → 永久挂死。**这是 MF-4 JobObject
/// 整树监管必要性的直接实证**：无整树终结时，孤儿不仅泄漏，还会把 pane 关闭
/// 路径一起拖死。此处直接 spawn ping（无包装）绕开，整树场景由 JobObject 测试覆盖。
/// 另注意：portable-pty 0.9.0 `do_kill`/`WinChildKiller::kill` 的 TerminateProcess
/// 错误判断写反（成功返回 Err）——`WinChild::kill` 用 `.ok()` 吞掉所以实际生效，
/// 但 `clone_killer()` 路径的返回值不可信，PaneHost 不得依赖它。
#[test]
fn kill_long_running_child_unblocks_reader() {
    let pty = native_pty_system()
        .openpty(default_size())
        .expect("openpty 失败");

    // ping -t 永不退出，确保 kill 是唯一终结路径；直接 spawn（见上方教训）。
    let mut cmd = CommandBuilder::new("ping");
    cmd.args(["-t", "127.0.0.1"]);
    let mut child = pty.slave.spawn_command(cmd).expect("spawn 失败");

    let reader = pty.master.try_clone_reader().expect("reader 失败");
    let writer = pty.master.take_writer().expect("writer 失败");
    let (read_thread, _saw_dsr) = spawn_reader_with_dsr_answer(reader, writer);

    // 给 ping 一点产出时间，证明进程真的在跑。
    thread::sleep(Duration::from_millis(1500));

    child.kill().expect("kill 失败");
    let status = child.wait().expect("wait 失败");
    assert!(!status.success(), "被 kill 的进程不应报成功退出");

    // slave + master 都 drop 才触发 ClosePseudoConsole → reader EOF（发现 #5）。
    drop(pty.slave);
    drop(pty.master);
    let total = read_thread.join().expect("读线程 panic").len();
    assert!(total > 0, "kill 前应已读到 ping 输出（证明阻塞读在工作）");
}

/// resize 不应报错（PaneSession::resize 的机制前提）。
#[test]
fn resize_pty_succeeds() {
    let pty = native_pty_system()
        .openpty(default_size())
        .expect("openpty 失败");
    pty.master
        .resize(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("resize 失败");
}
