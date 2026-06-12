//! D2 spike 探针：直接观察 ConPTY 行为（不经测试框架，避免输出吞噬）。
//!
//! 观察点：spawn 后输出流的原始字节（转义打印）、DSR `ESC[6n` 是否出现、
//! 应答 `ESC[1;1R` 后子进程是否退出、try_wait 状态演变。
//! 运行：`cargo run -p conmux --example conpty_probe`

#![cfg(windows)]

use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn esc(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b == 0x1b {
                "\\e".to_string()
            } else if (0x20..0x7f).contains(&b) {
                (b as char).to_string()
            } else if b == b'\r' {
                "\\r".to_string()
            } else if b == b'\n' {
                "\\n".to_string()
            } else {
                format!("\\x{b:02x}")
            }
        })
        .collect()
}

fn main() {
    let start = Instant::now();
    let t = |s: &str| println!("[{:6.2}s] {s}", start.elapsed().as_secs_f32());

    t("openpty…");
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    t("openpty OK");

    // kill2：精确复刻测试 kill_long_running_child_unblocks_reader 的序列，
    // 用于定位"探针通过但 cargo test 挂死"的差异点。
    if std::env::args().any(|a| a == "kill2") {
        let t = |s: &str| println!("[kill2] {s}");
        let pty = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("ping");
        cmd.args(["-t", "127.0.0.1"]);
        let mut child = pty.slave.spawn_command(cmd).expect("spawn");
        t("spawned");

        let mut reader = pty.master.try_clone_reader().expect("reader");
        let mut writer = pty.master.take_writer().expect("writer");
        // 与测试相同：writer 移进读线程、由读线程应答 DSR
        let read_thread = thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut all: Vec<u8> = Vec::new();
            let mut answered = false;
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        all.extend_from_slice(&buf[..n]);
                        if !answered && all.windows(4).any(|w| w == b"\x1b[6n") {
                            let _ = writer.write_all(b"\x1b[1;1R");
                            let _ = writer.flush();
                            answered = true;
                        }
                    }
                }
            }
            all.len()
        });

        thread::sleep(Duration::from_millis(1500));
        t("calling child.kill()…");
        let kr = child.kill();
        t(&format!("child.kill() returned: {kr:?}"));
        t("calling child.wait()…");
        let st = child.wait();
        t(&format!("child.wait() returned: {st:?}"));
        t("dropping master (slave 仍存活，writer 在读线程)…");
        drop(pty.master);
        t("master dropped; joining reader…");
        let total = read_thread.join().expect("join");
        t(&format!("reader joined, total={total}"));
        t("kill2 done");
        return;
    }

    let kill_mode = std::env::args().any(|a| a == "kill");
    let mut cmd = if kill_mode {
        let mut c = CommandBuilder::new("ping");
        c.args(["-t", "127.0.0.1"]);
        c
    } else {
        let mut c = CommandBuilder::new("cmd.exe");
        c.args(["/c", "echo conmux-probe"]);
        c
    };
    let _ = &mut cmd;
    t(if kill_mode {
        "spawn ping -t…"
    } else {
        "spawn cmd /c echo…"
    });
    let mut child = pty.slave.spawn_command(cmd).expect("spawn");
    t("spawn OK (returned)");
    let mut killer = child.clone_killer();

    let mut reader = pty.master.try_clone_reader().expect("reader");
    let mut writer = pty.master.take_writer().expect("writer");

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Vec::new()); // EOF 标记
                    break;
                }
                Ok(n) => {
                    let _ = tx.send(buf[..n].to_vec());
                }
            }
        }
    });

    let mut answered = false;
    let mut acc: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut killed = false;

    while Instant::now() < deadline {
        // kill 模式：拿到首批真实输出 + 已过 2s 后下手
        if kill_mode && !killed && answered && acc.len() > 100 && start.elapsed().as_secs_f32() > 2.0
        {
            t("calling killer.kill()…");
            let kr = killer.kill();
            t(&format!("killer.kill() returned: {kr:?}"));
            t("calling child.wait()…");
            let st = child.wait();
            t(&format!("child.wait() returned: {st:?}"));
            killed = true;
        }
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(chunk) if chunk.is_empty() => {
                t("reader EOF");
                break;
            }
            Ok(chunk) => {
                println!(
                    "[{:6.2}s] RECV {} bytes: {}",
                    start.elapsed().as_secs_f32(),
                    chunk.len(),
                    esc(&chunk)
                );
                acc.extend_from_slice(&chunk);
                if !answered && acc.windows(4).any(|w| w == b"\x1b[6n") {
                    t("DSR ESC[6n detected → reply ESC[1;1R");
                    writer.write_all(b"\x1b[1;1R").expect("write reply");
                    writer.flush().expect("flush");
                    answered = true;
                }
            }
            Err(_) => {
                // 每 300ms 报一次 child 状态
                match child.try_wait() {
                    Ok(Some(st)) => {
                        t(&format!("child exited: {st:?}"));
                        break;
                    }
                    Ok(None) => t("child still running (no output this tick)"),
                    Err(e) => t(&format!("try_wait err: {e}")),
                }
            }
        }
    }

    if kill_mode && killed {
        t("dropping writer…");
        drop(writer);
        t("dropping slave…");
        drop(pty.slave);
        t("dropping master (→ ClosePseudoConsole)…");
        drop(pty.master);
        t("master dropped OK");
        let eof_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(c) if c.is_empty() => {
                    t("reader EOF ✓");
                    break;
                }
                Ok(c) => println!("        RECV {} bytes after close", c.len()),
                Err(_) => {
                    if Instant::now() > eof_deadline {
                        t("NO reader EOF within 5s ✗");
                        break;
                    }
                }
            }
        }
        t(&format!("summary(kill): total_bytes={}, dsr_answered={answered}", acc.len()));
        t("probe done");
        return;
    }

    t(&format!(
        "summary: total_bytes={}, dsr_answered={answered}",
        acc.len()
    ));
    match child.try_wait() {
        Ok(Some(st)) => t(&format!("final child status: {st:?}")),
        Ok(None) => {
            t("child STILL RUNNING at probe end → kill");
            let _ = child.kill();
        }
        Err(e) => t(&format!("final try_wait err: {e}")),
    }
    t("probe done");
}
