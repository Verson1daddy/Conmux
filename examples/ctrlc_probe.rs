//! Ctrl+C 退出语义探针：真实 claude 在 ConPTY 内，注入 \x03 ×2（间隔可调），
//! 观察是否退出。回答"前端修复双击武装后，Ctrl+C 到底能不能退出 claude"。
//!
//! 运行：`cargo run -p conmux --example ctrlc_probe`

#![cfg(windows)]

use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn main() {
    let start = Instant::now();
    let t = |s: &str| eprintln!("[{:6.2}s] {s}", start.elapsed().as_secs_f32());

    let pty = native_pty_system()
        .openpty(PtySize { rows: 30, cols: 100, pixel_width: 0, pixel_height: 0 })
        .expect("openpty");

    let mut cmd = CommandBuilder::new("cmd.exe");
    cmd.args(["/c", "claude"]);
    let mut child = pty.slave.spawn_command(cmd).expect("spawn claude");
    t("claude spawned");

    let mut reader = pty.master.try_clone_reader().expect("reader");
    let mut writer = pty.master.take_writer().expect("writer");

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Vec::new());
                    break;
                }
                Ok(n) => {
                    let _ = tx.send(buf[..n].to_vec());
                }
            }
        }
    });

    let mut answered = false;
    let mut total = 0usize;
    // 等 claude UI 稳定（静默 2s 或最多 20s）
    let mut last = Instant::now();
    let begin = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(c) if c.is_empty() => {
                t("EOF before injection?!");
                return;
            }
            Ok(c) => {
                total += c.len();
                last = Instant::now();
                if !answered && c.windows(4).any(|w| w == b"\x1b[6n") {
                    writer.write_all(b"\x1b[1;1R").unwrap();
                    writer.flush().unwrap();
                    answered = true;
                    t("DSR answered");
                }
            }
            Err(_) => {}
        }
        if (last.elapsed() > Duration::from_secs(2) && total > 1000)
            || begin.elapsed() > Duration::from_secs(20)
        {
            break;
        }
    }
    t(&format!("claude UI ready, bytes={total}"));

    // 注入 \x03 两次，间隔 400ms（人手双击节奏）
    writer.write_all(b"\x03").unwrap();
    writer.flush().unwrap();
    t("sent ^C #1");
    thread::sleep(Duration::from_millis(400));
    writer.write_all(b"\x03").unwrap();
    writer.flush().unwrap();
    t("sent ^C #2");

    // 观察 10s：进程退出？
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Some(st)) = child.try_wait() {
            t(&format!("✅ claude EXITED after ^C^C: {st:?}"));
            drop(writer);
            drop(pty.slave);
            drop(pty.master);
            return;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(c) if c.is_empty() => {
                t("reader EOF (likely exited)");
            }
            Ok(c) => {
                let s = String::from_utf8_lossy(&c);
                if s.contains("again to exit") || s.contains("Press") {
                    t(&format!("claude says: …{}…", s.chars().take(120).collect::<String>()));
                }
            }
            Err(_) => {}
        }
        if Instant::now() > deadline {
            break;
        }
    }
    t("❌ claude still running 10s after ^C^C — byte injection cannot exit it");
    let _ = child.kill();
    let _ = child.wait();
    t("cleaned up");
}
