//! conmux 瘦客户端（M2a，仅 Windows）：连接（自动拉起）+ 握手 + 请求-应答。
//!
//! CLI / GUI 壳 / 第三方前端共用。**自动拉起**（D-2，tmux 心智）：连接管道失败（无 daemon）
//! → detached spawn `conmux daemon` → 有限退避重试（总预算 ≤3s）。单实例由 daemon 侧
//! `FILE_FLAG_FIRST_PIPE_INSTANCE` 保证——竞态下多个 auto-spawn 只有一个 daemon 存活，
//! 其余 bind 失败退出，客户端连上存活者。
//!
//! **客户端反冒充（红队 M-4，部分）**：M2a 先打通连接 + 取服务端进程身份的钩子位
//! （`PipeStream` 侧由 daemon 取客户端身份）；签名校验主路径（`GetNamedPipeServerProcessId`
//! → Authenticode 比对）登记为 M2c 加固项（设计 D-3 I-2 客户端侧），此处先记 TODO 不放行降级。

use std::iter::once;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
    PROCESS_INFORMATION, STARTUPINFOW,
};

use crate::event::MuxNotify;
use crate::pipe::{try_connect, ConnectOutcome, PipeReader, PipeStream, PipeWriter};
use crate::protocol::{MuxOp, MuxPayload, MuxReply, MuxRequest, WireFrame, PROTOCOL_VERSION};
use crate::types::PaneId;
use crate::wire::{read_frame, write_frame, WireError};
use crate::ConmuxError;

/// 自动拉起后等待 daemon 就绪的总预算。
const SPAWN_READY_BUDGET: Duration = Duration::from_secs(3);

/// 一个已握手的客户端连接。
pub struct Client {
    stream: PipeStream,
    next_cid: u64,
}

impl Client {
    /// 连接当前用户 daemon；无 daemon ⇒ 自动拉起后重试（CLI 默认入口）。
    pub fn connect_or_spawn() -> Result<Self, ConmuxError> {
        let name = crate::pipe::default_pipe_name()?;
        Self::connect_named(&name, true)
    }

    /// 连接指定管道名，**不自动拉起**（测试 / 嵌入者已自管 daemon 生命周期时用）。
    pub fn connect(name: &str) -> Result<Self, ConmuxError> {
        Self::connect_named(name, false)
    }

    fn connect_named(name: &str, allow_spawn: bool) -> Result<Self, ConmuxError> {
        match try_connect(name, 1000)? {
            ConnectOutcome::Connected(s) => return Self::handshake(s),
            ConnectOutcome::NoDaemon => {
                if !allow_spawn {
                    return Err(ConmuxError::PtyError {
                        message: format!("daemon 未运行（管道 {name} 不存在），且未启用自动拉起"),
                    });
                }
            }
        }
        // 自动拉起：detached spawn 当前可执行文件的 `daemon` 子命令。
        spawn_daemon_detached()?;
        let deadline = Instant::now() + SPAWN_READY_BUDGET;
        loop {
            std::thread::sleep(Duration::from_millis(100));
            match try_connect(name, 500)? {
                ConnectOutcome::Connected(s) => return Self::handshake(s),
                ConnectOutcome::NoDaemon => {}
            }
            if Instant::now() >= deadline {
                return Err(ConmuxError::PtyError {
                    message: "自动拉起 daemon 后等待就绪超时（3s）".into(),
                });
            }
        }
    }

    /// D-4 握手：发 Hello → 收 HelloAck（版本严格相等）。
    fn handshake(mut stream: PipeStream) -> Result<Self, ConmuxError> {
        let hello = WireFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_kind: "conmux-cli".into(),
        };
        write_frame(&mut stream, &hello).map_err(wire_to_conmux)?;
        match read_frame(&mut stream).map_err(wire_to_conmux)? {
            WireFrame::HelloAck {
                protocol_version, ..
            } => {
                if protocol_version != PROTOCOL_VERSION {
                    return Err(ConmuxError::Unsupported {
                        message: format!(
                            "协议版本不匹配：客户端 {PROTOCOL_VERSION} vs daemon {protocol_version}"
                        ),
                    });
                }
                // M2c-3 反冒充（I-2 客户端侧，红队 M-4）：核验 daemon 进程身份。
                verify_server_identity(&stream);
                Ok(Self {
                    stream,
                    next_cid: 1,
                })
            }
            other => Err(ConmuxError::PtyError {
                message: format!("握手应答非 HelloAck：{other:?}"),
            }),
        }
    }

    /// 单次请求-应答。M2a 无订阅，故收到 `Notify` 一律忽略（M2b attach 时改为按 D-6 缓冲拼接）。
    pub fn request(&mut self, op: MuxOp) -> Result<MuxPayload, ConmuxError> {
        let cid = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1);
        let req = WireFrame::Request(MuxRequest {
            correlation_id: cid,
            op,
        });
        write_frame(&mut self.stream, &req).map_err(wire_to_conmux)?;
        loop {
            match read_frame(&mut self.stream).map_err(wire_to_conmux)? {
                WireFrame::Reply(reply) => {
                    if reply.correlation_id() != cid {
                        continue; // 非本请求应答（M2a 单连接顺序往返，不应发生）
                    }
                    return match reply {
                        MuxReply::Ok { payload, .. } => Ok(payload),
                        MuxReply::Err { error, .. } => Err(error),
                    };
                }
                WireFrame::Notify(_) => continue, // 控制态忽略异步事件
                other => {
                    return Err(ConmuxError::PtyError {
                        message: format!("非预期帧方向（客户端只应收 Reply/Notify）：{other:?}"),
                    })
                }
            }
        }
    }

    /// Attach 一个 pane（D-6）：发 `Attach` → 收 `AttachSnapshot`（**缓冲**期间到达的 live
    /// `PaneOutput`，客户端拼接契约 M-1）→ 拆连接为流式会话。消费 `Client`（连接转 attach 态）。
    ///
    /// 返回 [`Attached`]：快照（preamble/history/last_seq/state）+ 缓冲帧（已按 seq 升序、
    /// 去重 `seq>last_seq`）+ [`AttachSession`]（收 live 输出 + 注入 stdin）。重建画面顺序 =
    /// 喂 preamble → 喂 history → 喂 buffered → 循环 `session.recv_output()`。
    pub fn attach(self, pane_id: &PaneId) -> Result<Attached, ConmuxError> {
        let Client {
            mut stream,
            mut next_cid,
        } = self;
        let cid = next_cid;
        next_cid = next_cid.wrapping_add(1);
        write_frame(
            &mut stream,
            &WireFrame::Request(MuxRequest {
                correlation_id: cid,
                op: MuxOp::Attach {
                    pane_id: pane_id.clone(),
                },
            }),
        )
        .map_err(wire_to_conmux)?;

        // 缓冲快照前到达的本 pane live 帧（D-6 拼接契约：未来帧不得先于历史渲染）。
        let mut buffered: Vec<(u64, Vec<u8>)> = Vec::new();
        let (mode_preamble, history, last_seq, pane_state) = loop {
            match read_frame(&mut stream).map_err(wire_to_conmux)? {
                WireFrame::Reply(MuxReply::Ok {
                    correlation_id,
                    payload:
                        MuxPayload::AttachSnapshot {
                            mode_preamble_b64,
                            history_b64,
                            last_seq,
                            pane_state,
                        },
                }) if correlation_id == cid => {
                    break (
                        b64_decode(&mode_preamble_b64)?,
                        b64_decode(&history_b64)?,
                        last_seq,
                        pane_state,
                    );
                }
                WireFrame::Reply(MuxReply::Err {
                    correlation_id,
                    error,
                }) if correlation_id == cid => return Err(error),
                WireFrame::Notify(MuxNotify::PaneOutput {
                    pane_id: pid,
                    seq,
                    data,
                }) if &pid == pane_id => buffered.push((seq, data)),
                // 其它 pane 事件 / 非本请求应答：attach 期忽略。
                _ => {}
            }
        };

        // 缓冲帧按 seq 升序 + 去重（只留 seq>last_seq；≤last_seq 已含于 history）。
        buffered.sort_by_key(|(s, _)| *s);
        buffered.retain(|(s, _)| *s > last_seq);

        let (reader, writer) = stream.split()?;
        Ok(Attached {
            mode_preamble,
            history,
            last_seq,
            pane_state,
            buffered,
            session: AttachSession {
                reader,
                writer,
                pane_id: pane_id.clone(),
                next_cid,
            },
        })
    }
}

/// Attach 握手结果（D-6）：原子快照 + attach 前缓冲的 live 帧 + 流式会话。
pub struct Attached {
    /// 非默认 VT 模式位合成前导（先喂）。
    pub mode_preamble: Vec<u8>,
    /// ring 原始 VT 历史（次喂）。
    pub history: Vec<u8>,
    /// 快照序号高水位（live 去重锚）。
    pub last_seq: u64,
    pub pane_state: crate::types::PaneState,
    /// attach 前缓冲、`seq>last_seq` 的 live 帧（升序，喂完 history 后喂这些）。
    pub buffered: Vec<(u64, Vec<u8>)>,
    /// 流式会话（继续收 live 输出 + 注入 stdin）。
    pub session: AttachSession,
}

/// Attach 流式事件。
pub enum AttachEvent {
    /// pane 输出（`seq` 单调，客户端可据 `> last_seq` 去重）。
    Output { seq: u64, data: Vec<u8> },
    /// pane 进程退出。
    Exited { exit_code: Option<i32> },
}

/// Attach 流式会话：reader 收 live 输出，writer 注入 stdin（经唯一写链 UserDirect）。
/// reader/writer 各自独立事件（重叠 I/O），可分别交不同线程并发（attach UI：渲染线程 + stdin 线程）。
pub struct AttachSession {
    reader: PipeReader,
    writer: PipeWriter,
    pane_id: PaneId,
    next_cid: u64,
}

impl AttachSession {
    /// 读下一个本 pane 事件（阻塞）。跳过 stdin ack（Reply）与其它 pane 帧。
    /// 返回 None = 连接关闭。
    pub fn recv_output(&mut self) -> Option<AttachEvent> {
        loop {
            match read_frame(&mut self.reader) {
                Ok(WireFrame::Notify(MuxNotify::PaneOutput {
                    pane_id, seq, data,
                })) if pane_id == self.pane_id => {
                    return Some(AttachEvent::Output { seq, data })
                }
                Ok(WireFrame::Notify(MuxNotify::PaneExited {
                    pane_id,
                    exit_code,
                })) if pane_id == self.pane_id => {
                    return Some(AttachEvent::Exited { exit_code })
                }
                Ok(_) => continue, // 其它 pane / stdin ack reply
                Err(_) => return None,
            }
        }
    }

    /// 拆出 reader 与一个可注入 stdin 的句柄（attach UI：渲染线程拿 reader，主线程拿 sender）。
    pub fn into_split(self) -> (AttachReader, AttachSender) {
        (
            AttachReader {
                reader: self.reader,
                pane_id: self.pane_id.clone(),
            },
            AttachSender {
                writer: self.writer,
                pane_id: self.pane_id,
                next_cid: self.next_cid,
            },
        )
    }

    /// 注入 stdin 字节到 pane（经唯一写链，UserDirect）。
    pub fn send_input(&mut self, data: &[u8]) -> Result<(), ConmuxError> {
        let cid = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1);
        write_frame(
            &mut self.writer,
            &WireFrame::Request(MuxRequest {
                correlation_id: cid,
                op: MuxOp::Send {
                    pane_id: self.pane_id.clone(),
                    data: data.to_vec(),
                },
            }),
        )
        .map_err(wire_to_conmux)
    }
}

/// attach 渲染半（交渲染线程，循环 `recv_output`）。
pub struct AttachReader {
    reader: PipeReader,
    pane_id: PaneId,
}
impl AttachReader {
    pub fn recv_output(&mut self) -> Option<AttachEvent> {
        loop {
            match read_frame(&mut self.reader) {
                Ok(WireFrame::Notify(MuxNotify::PaneOutput {
                    pane_id, seq, data,
                })) if pane_id == self.pane_id => {
                    return Some(AttachEvent::Output { seq, data })
                }
                Ok(WireFrame::Notify(MuxNotify::PaneExited {
                    pane_id,
                    exit_code,
                })) if pane_id == self.pane_id => {
                    return Some(AttachEvent::Exited { exit_code })
                }
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }
}

/// attach 注入半（交 stdin 线程，转发键入）。
pub struct AttachSender {
    writer: PipeWriter,
    pane_id: PaneId,
    next_cid: u64,
}
impl AttachSender {
    pub fn send_input(&mut self, data: &[u8]) -> Result<(), ConmuxError> {
        let cid = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1);
        write_frame(
            &mut self.writer,
            &WireFrame::Request(MuxRequest {
                correlation_id: cid,
                op: MuxOp::Send {
                    pane_id: self.pane_id.clone(),
                    data: data.to_vec(),
                },
            }),
        )
        .map_err(wire_to_conmux)
    }

    /// 调整 pane 尺寸（D-9 resize 联动：attach 起手 + 控制台尺寸变化时跟随）。
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), ConmuxError> {
        let cid = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1);
        write_frame(
            &mut self.writer,
            &WireFrame::Request(MuxRequest {
                correlation_id: cid,
                op: MuxOp::Resize {
                    pane_id: self.pane_id.clone(),
                    size: crate::types::PaneSize { rows, cols },
                },
            }),
        )
        .map_err(wire_to_conmux)
    }
}

/// 反冒充核验（I-2 客户端侧，红队 M-4）：daemon 进程映像应与本客户端同主体。
///
/// **dev 退化为 image path 比对**（conmux CLI 自拉起/同装时 daemon == 本 exe，匹配即静默）；
/// 不匹配/不可得 ⇒ **报警**（threat model：同用户抢注最坏退化为 DoS，不产生静默劫持——校验
/// 职责是抬高门槛 + 可审计 + 报警，非硬断）。**生产加固登记**：Authenticode 签名同主体校验
/// （WinVerifyTrust）替代路径比对，对第三方客户端按签名而非路径判定。
fn verify_server_identity(stream: &PipeStream) {
    let server_image = stream
        .server_process_id()
        .and_then(crate::pipe::process_image_path);
    let self_exe = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_lowercase());
    match (server_image, self_exe) {
        (Some(srv), Some(me)) if srv.to_lowercase() != me => {
            eprintln!(
                "conmux 警告：daemon 进程映像（{srv}）与本客户端（{me}）不一致——可能被冒充或为\
                 第三方 daemon。生产应以 Authenticode 签名同主体校验；如非预期请 `conmux kill-server` 后重试。"
            );
        }
        (None, _) => {
            eprintln!("conmux 警告：无法核验 daemon 进程身份（GetNamedPipeServerProcessId 失败）。");
        }
        _ => {} // 匹配 = 同 exe（conmux CLI 自拉起 / 同装），静默。
    }
}

fn b64_decode(s: &str) -> Result<Vec<u8>, ConmuxError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .map_err(|e| ConmuxError::SerializationError {
            message: format!("base64 解码失败: {e}"),
        })
}

/// detached spawn 当前可执行文件的 `daemon` 子命令。
///
/// **必须 `bInheritHandles=FALSE`**（不用 std `Command`）：std Command 在 Windows 上以
/// `bInheritHandles=TRUE` 启动子进程，会把父进程**全部可继承句柄**复制进 daemon——
/// 包括调用方 stdout 若被重定向为管道（如 `$x = & conmux new` 捕获），daemon 持其副本
/// 会让父读端永不见 EOF（实测挂死）。CreateProcessW + 不继承句柄根除此泄漏；daemon
/// 自己经 stdio = 控制台/无（DETACHED_PROCESS|CREATE_NO_WINDOW），不依赖父句柄。
fn spawn_daemon_detached() -> Result<(), ConmuxError> {
    use std::os::windows::ffi::OsStrExt;
    let exe = std::env::current_exe().map_err(|e| ConmuxError::PtyError {
        message: format!("取当前可执行文件路径失败: {e}"),
    })?;
    let exe_wide: Vec<u16> = exe.as_os_str().encode_wide().chain(once(0)).collect();
    // 命令行（CreateProcessW 可改写，故 mutable）：`"<exe>" daemon`。
    let mut cmdline: Vec<u16> = format!("\"{}\" daemon", exe.display())
        .encode_utf16()
        .chain(once(0))
        .collect();

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // SAFETY: exe_wide/cmdline 以 null 结尾；si/pi 已零初始化且 cb 正确；
    // bInheritHandles=FALSE（0）——daemon 不继承父任何句柄（关键）。
    let ok = unsafe {
        CreateProcessW(
            exe_wide.as_ptr(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0, // bInheritHandles = FALSE
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        return Err(ConmuxError::PtyError {
            message: format!("自动拉起 conmux daemon 失败（CreateProcessW GetLastError={err}）"),
        });
    }
    // 不等待 daemon；立即关闭进程/线程句柄（daemon 经命名管道独立运行）。
    unsafe {
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }
    Ok(())
}

fn wire_to_conmux(e: WireError) -> ConmuxError {
    match e {
        WireError::Json(je) => ConmuxError::SerializationError {
            message: je.to_string(),
        },
        other => ConmuxError::PtyError {
            message: other.to_string(),
        },
    }
}
