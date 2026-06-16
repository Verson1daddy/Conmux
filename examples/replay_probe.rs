//! M2 spike 探针：采集真实 ConPTY 输出流，供 VT 重放保真度比对（Node + xterm headless）。
//!
//! 场景（一次会话连续驱动）：
//! - S1 平滚输出（200 行）——朴素 shell 历史
//! - S2 进出 alt-screen（TUI 开-画-关-回主屏）
//! - S3 进 alt-screen 后**停留**（模拟 TUI 运行中 detach：claude 在跑时杀客户端）
//!
//! 输出：`<outdir>/full.bin`（全量原始字节 = 活客户端从 t=0 看到的流）。
//! ring 窗口/截断重放由 Node 侧切片模拟（LineIndexedBuffer 语义 = 最近 ≤1MB 字节窗口，
//! TUI 无换行流下窗口起点等效任意字节）。
//!
//! 运行：`cargo run -p conmux --example replay_probe -- <outdir>`

#![cfg(windows)]

use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn main() {
    let outdir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&outdir).expect("outdir");
    let start = Instant::now();
    let t = |s: &str| eprintln!("[{:6.2}s] {s}", start.elapsed().as_secs_f32());

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new("powershell.exe");
    cmd.args(["-NoProfile", "-NoLogo"]);
    let mut child = pty.slave.spawn_command(cmd).expect("spawn powershell");
    t("powershell spawned");

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

    let mut acc: Vec<u8> = Vec::new();
    let mut answered = false;

    // 收流直到静默 quiet_ms 或超时。
    let drain = |acc: &mut Vec<u8>,
                     writer: &mut Box<dyn Write + Send>,
                     answered: &mut bool,
                     quiet_ms: u64,
                     max_ms: u64| {
        let begin = Instant::now();
        let mut last = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => {
                    acc.extend_from_slice(&chunk);
                    last = Instant::now();
                    if !*answered && acc.windows(4).any(|w| w == b"\x1b[6n") {
                        writer.write_all(b"\x1b[1;1R").expect("dsr reply");
                        writer.flush().expect("flush");
                        *answered = true;
                    }
                }
                Err(_) => {}
            }
            if last.elapsed() > Duration::from_millis(quiet_ms)
                || begin.elapsed() > Duration::from_millis(max_ms)
            {
                break;
            }
        }
    };

    // 等 banner + 提示符稳定
    drain(&mut acc, &mut writer, &mut answered, 1200, 15_000);
    t(&format!("prompt ready, bytes={}", acc.len()));

    let send = |writer: &mut Box<dyn Write + Send>, line: &str| {
        writer.write_all(line.as_bytes()).expect("send");
        writer.write_all(b"\r").expect("cr");
        writer.flush().expect("flush");
    };

    // S1 平滚
    send(&mut writer, "1..200 | ForEach-Object { \"S1 line $_\" }");
    drain(&mut acc, &mut writer, &mut answered, 1200, 20_000);
    let s1_end = acc.len();
    t(&format!("S1 done, bytes={s1_end}"));

    // S2 进出 alt-screen（开-清屏-画 10 行-停 300ms-关-回主屏标记）
    send(
        &mut writer,
        "$e=[char]27; Write-Host \"$e[?1049h$e[2J$e[H\" -NoNewline; \
         1..10 | ForEach-Object { Write-Host \"$e[$_;1HTUI-A ROW $_\" -NoNewline }; \
         Start-Sleep -Milliseconds 300; Write-Host \"$e[?1049l\" -NoNewline; \"S2 BACK ON MAIN\"",
    );
    drain(&mut acc, &mut writer, &mut answered, 1200, 20_000);
    let s2_end = acc.len();
    t(&format!("S2 done, bytes={s2_end}"));

    // S3 进 alt-screen 并停留（含隐藏光标 ?25l + 着色行——detach 时 TUI 活跃态）
    send(
        &mut writer,
        "$e=[char]27; Write-Host \"$e[?1049h$e[2J$e[H$e[?25l\" -NoNewline; \
         1..8 | ForEach-Object { Write-Host \"$e[$_;1H$e[3$(($_ % 7 + 1))mTUI-B ROW $_$e[0m\" -NoNewline }; \
         Write-Host \"$e[10;1HTUI-B STATUS BAR\" -NoNewline",
    );
    drain(&mut acc, &mut writer, &mut answered, 1500, 20_000);
    t(&format!("S3 done (staying in alt-screen), bytes={}", acc.len()));

    // 落盘
    let full = format!("{outdir}/full.bin");
    std::fs::write(&full, &acc).expect("write full.bin");
    std::fs::write(
        format!("{outdir}/markers.json"),
        format!("{{\"s1_end\":{s1_end},\"s2_end\":{s2_end},\"total\":{}}}", acc.len()),
    )
    .expect("write markers");
    t(&format!("wrote {full} ({} bytes)", acc.len()));

    // 清理：杀 shell（停在 alt-screen 内，不发退出命令——模拟 detach 后整树终结）
    let _ = child.kill();
    let _ = child.wait();
    drop(writer);
    drop(pty.slave);
    drop(pty.master);
    t("probe done");
}
