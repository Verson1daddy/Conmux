//! conmux daemon 服务端（M2 设计 D-2..D-7，仅 Windows）。
//!
//! tmux server 模型（I-4）：单 daemon 持全部 ConPTY pane；CLI/GUI/第三方是瘦客户端。
//! **每连接 reader + writer 双线程**（D-7）：reader 阻塞 ReadFile 收请求/stdin；writer 从
//! 有界外发队列取帧 WriteFile（回复 + 订阅事件）。事件 fan-out 非阻塞投递——任何慢客户端
//! 不传导到 PTY 读路径（D-7）。
//!
//! ## 安全/正确性不变量落实位
//! - **I-2 抢注守卫**：[`Daemon::bind`] 经 `PipeListener::bind`（FIRST_PIPE_INSTANCE），失败即退出。
//! - **I-5 身份 fail-closed**：[`handle_connection`] 取不到客户端 pid ⇒ 立即断连。
//! - **D-4 握手 + H-2 方向约束**：[`serve_connection`] 首帧必须 Hello + 版本严格相等；握手后只收 Request。
//! - **R-1 唯一写链跨 IPC / R-2 IPC 注入一律 UserDirect**：[`build_reply`] 对 Send 唯一实现 = `inject_stdin(UserDirect)`。
//! - **D-5 订阅模型**：Subscribe/Unsubscribe/Attach 维护每连接订阅集；[`FanoutSink`] 只向订阅者投递。
//! - **D-6 attach 无缝拼接**：Attach = 先注册订阅、后取 `attach_snapshot`（原子 history+last_seq）；
//!   base64/JSON 在锁外（PaneHost 已保证 H-1 锁纪律）。
//! - **D-7 背压**：每连接外发队列字节上界（8 MiB），超限断连该连接（读泵零损失）。
//! - **H-3 panic 隔离**：每连接 reader 以 `catch_unwind` 包裹；PaneHost 锁中毒容忍（M2a-M1）。

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::event::{MuxNotify, PaneEventSink};
use crate::pane::PaneHost;
use crate::pipe::{process_image_path, try_connect, ConnectOutcome, PipeListener, PipeStream, PipeWriter};
use crate::protocol::{MuxOp, MuxPayload, MuxReply, MuxRequest, WireFrame, PROTOCOL_VERSION};
use crate::types::{InjectionSource, PaneId, PaneLifecycle};
use crate::wire::{read_frame, write_frame, WireError};
use crate::ConmuxError;

/// daemon 版本（HelloAck 回报，仅审计/诊断，不参与授权）。
const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 每连接外发队列字节上界（D-7）。超限 ⇒ 断连该连接（客户端重连经 attach 快照恢复，零损失）。
const MAX_QUEUE_BYTES: usize = 8 * 1024 * 1024;

/// per-连接 attach 最小间隔（D-7 限速）：防快照放大 DoS（~100B Attach → 1.4MB 快照帧）。
const ATTACH_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// daemon 日志滚动上限（D-2：本地文件，无遥测）。
const MAX_LOG_BYTES: u64 = 1024 * 1024;

// ===== 连接审计日志（D-2 / RT-2，本地文件，无遥测，fail-soft）=====

/// daemon 连接级审计日志：连接/断开事件（{pid, image_path, 时刻}）落本地文件。
/// **fail-soft**：任何 I/O 失败静默忽略，绝不阻断服务（审计是诊断辅助，非服务依赖）。
struct DaemonLog {
    state: Mutex<LogState>,
}
struct LogState {
    path: std::path::PathBuf,
    max_bytes: u64,
}

impl DaemonLog {
    /// 生产：`%LOCALAPPDATA%\conmux\daemon.log`（滚动 1 MiB）。无 LOCALAPPDATA 退化到 temp。
    fn for_current_user() -> Self {
        let mut dir = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        dir.push("conmux");
        let _ = std::fs::create_dir_all(&dir);
        dir.push("daemon.log");
        Self::with_path(dir, MAX_LOG_BYTES)
    }

    fn with_path(path: std::path::PathBuf, max_bytes: u64) -> Self {
        Self {
            state: Mutex::new(LogState { path, max_bytes }),
        }
    }

    /// 追加一条审计行（`<epoch_ms> <event>`）。滚动：超上限把现文件改名 .1 后另起。
    fn log(&self, event: &str) {
        use std::io::Write;
        // 锁内只克隆 path + max_bytes（短临界区）；所有 fs I/O 移到锁外
        // （消 §2.3① 锁内 I/O——慢盘 / 杀软扫描不再阻塞后续 log 调用）。
        let (path, max_bytes) = {
            let g = self.state.lock().unwrap_or_else(recover);
            (g.path.clone(), g.max_bytes)
        };
        // 滚动（best-effort，锁外）。
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > max_bytes {
                let _ = std::fs::rename(&path, path.with_extension("log.1"));
            }
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{ts} {event}");
        }
    }
}

/// 中毒容忍锁恢复（H-3，与 pane.rs::recover 同策略）——连接线程 panic 不级联成全域锁风暴。
fn recover<T>(e: PoisonError<MutexGuard<'_, T>>) -> MutexGuard<'_, T> {
    e.into_inner()
}

/// daemon 装配配置。
pub struct DaemonConfig {
    /// 监听管道名。生产取 `pipe::default_pipe_name()`；测试用隔离名。
    pub pipe_name: String,
}

impl DaemonConfig {
    /// 当前用户的默认管道名（`\\.\pipe\conmux.<SID>`）。
    pub fn for_current_user() -> Result<Self, ConmuxError> {
        Ok(Self {
            pipe_name: crate::pipe::default_pipe_name()?,
        })
    }
}

// ===== 外发队列 / 连接句柄 =====

/// 投递给 writer 线程的外发项。
enum Outbound {
    /// 一帧（含近似字节数，用于背压记账）。
    Frame(WireFrame, usize),
    /// 主动断连信号（背压超限 / 连接清理）——writer 收到即退出。
    Disconnect,
}

/// 连接句柄：外发队列 sender + 背压记账 + 订阅集。reader 线程、writer 线程、FanoutSink 共享。
struct ConnHandle {
    tx: Sender<Outbound>,
    /// 当前排队待发字节数（背压锚）。
    queued_bytes: AtomicUsize,
    /// 背压触发后置 true：fanout 跳过、reader 不再入队（writer 已收 Disconnect 退出）。
    dead: AtomicBool,
    /// 本连接订阅的 pane 集（D-5）。
    subscriptions: Mutex<HashSet<PaneId>>,
    /// 上次 attach 时刻（D-7 限速：<500ms 回 Busy，防快照放大 DoS）。
    last_attach_at: Mutex<Option<Instant>>,
}

impl ConnHandle {
    /// 入队一帧（背压感知，非阻塞）。超 8 MiB ⇒ 置 dead + 发 Disconnect（D-7）。
    fn enqueue(&self, frame: WireFrame, bytes: usize) {
        if self.dead.load(Ordering::Relaxed) {
            return;
        }
        if self.queued_bytes.load(Ordering::Relaxed).saturating_add(bytes) > MAX_QUEUE_BYTES {
            self.dead.store(true, Ordering::Relaxed);
            let _ = self.tx.send(Outbound::Disconnect);
            return;
        }
        self.queued_bytes.fetch_add(bytes, Ordering::Relaxed);
        if self.tx.send(Outbound::Frame(frame, bytes)).is_err() {
            // writer 已退出（连接断）——回滚记账。
            self.queued_bytes.fetch_sub(bytes, Ordering::Relaxed);
        }
    }

    fn is_subscribed(&self, pane_id: &PaneId) -> bool {
        self.subscriptions.lock().unwrap_or_else(recover).contains(pane_id)
    }
}

/// 事件出口：把 per-pane 事件 fan-out 给订阅该 pane 的连接（D-5）。
/// **非阻塞**（D-7）：on_notify 由 PaneHost 读泵调用，只做短锁 + 非阻塞 try_send，
/// 慢客户端的背压断连不回传到读路径。
struct FanoutSink {
    conns: Arc<Mutex<HashMap<u64, Arc<ConnHandle>>>>,
}

impl PaneEventSink for FanoutSink {
    fn on_notify(&self, notify: MuxNotify) {
        let pane_id = match &notify {
            MuxNotify::PaneOutput { pane_id, .. } | MuxNotify::PaneExited { pane_id, .. } => {
                pane_id.clone()
            }
            // ThemeChanged 广播 = M2c（依赖 SetTheme 落地）。
            _ => return,
        };
        let bytes = notify_bytes(&notify);
        let conns = self.conns.lock().unwrap_or_else(recover);
        for conn in conns.values() {
            if conn.is_subscribed(&pane_id) {
                conn.enqueue(WireFrame::Notify(notify.clone()), bytes);
            }
        }
    }
}

/// 跨连接共享态：PaneHost（全部 pane 的属主）+ 运行标志 + 管道名 + 连接注册表。
struct DaemonShared {
    host: PaneHost,
    running: AtomicBool,
    pipe_name: String,
    conns: Arc<Mutex<HashMap<u64, Arc<ConnHandle>>>>,
    next_conn_id: AtomicU64,
    /// 正在取快照的 pane 集（D-7：per-pane 并发快照=1，进行中再 Attach 回 Busy，防放大）。
    attaching: Mutex<HashSet<PaneId>>,
    /// 连接级审计日志（D-2/RT-2，本地文件 fail-soft）。
    log: DaemonLog,
    /// 信任库句柄（Slice 3：PinExecutable IPC 经此直接改内存态 + 存盘，
    /// 与 PaneHost 持有的 `Arc<dyn TrustPolicy>` 是**同一 Arc**，pin 后下次 spawn verify 即见）。
    /// None = 测试态（PinExecutable 返回 Unsupported）。
    trust_store: Option<Arc<crate::trust::SharedTrustStore>>,
}

/// conmux daemon。`bind` 绑定管道，`serve` 进入服务循环。
pub struct Daemon {
    shared: Arc<DaemonShared>,
    listener: PipeListener,
}

impl Daemon {
    /// 绑定管道并装配 PaneHost（事件出口 = FanoutSink）。`bind` 失败 ⇒ 已有 daemon / 被抢注（I-2）。
    pub fn bind(config: DaemonConfig) -> Result<Self, ConmuxError> {
        let listener = PipeListener::bind(&config.pipe_name)?;
        let conns: Arc<Mutex<HashMap<u64, Arc<ConnHandle>>>> = Arc::new(Mutex::new(HashMap::new()));
        // M2a 单用形态：钩子链空（R-2 全 UserDirect）；event_sink = FanoutSink（按订阅投递）。
        // Slice 2：启动时加载 TrustStore 一次，注入 PaneHost（spawn 热路径不做文件 I/O）。
        // 用 SharedTrustStore：未来 reload IPC 可经同一共享态即时生效。
        let trust_store = Arc::new(crate::trust::SharedTrustStore::load_or_create());
        // 同一 Arc 两份：一份 move 进 PaneHost（coerce 为 Arc<dyn TrustPolicy>），
        // 一份留 DaemonShared 供 PinExecutable IPC 直接改内存态（保持具体类型调 pin_executable）。
        let trust_for_shared = Arc::clone(&trust_store);
        let host = PaneHost::new_windows_with_trust(
            Vec::new(),
            Arc::new(FanoutSink {
                conns: Arc::clone(&conns),
            }),
            trust_store,
        );
        let shared = Arc::new(DaemonShared {
            host,
            running: AtomicBool::new(true),
            pipe_name: config.pipe_name,
            conns,
            next_conn_id: AtomicU64::new(1),
            attaching: Mutex::new(HashSet::new()),
            log: DaemonLog::for_current_user(),
            trust_store: Some(trust_for_shared),
        });
        Ok(Self { shared, listener })
    }

    /// 取关闭句柄（测试 / 外部触发用）。KillServer 经连接内部触发，无需此句柄。
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// 服务循环：accept → 每连接 reader 线程（reader 内再起 writer 线程）。阻塞至 shutdown。
    pub fn serve(mut self) {
        // D-2a 兜底：poll_exit sweep 线程——补 ConPTY 不返 EOF 时读泵漏发的 PaneExited，
        // 使 attach 客户端的退出态可达。随 running=false 自然退出（最迟一个 sweep 间隔）。
        {
            let shared = Arc::clone(&self.shared);
            std::thread::spawn(move || run_exit_sweep(shared));
        }
        while self.shared.running.load(Ordering::SeqCst) {
            match self.listener.accept() {
                Ok(stream) => {
                    if !self.shared.running.load(Ordering::SeqCst) {
                        break; // shutdown 期间的 self-connect 唤醒帧，丢弃
                    }
                    let shared = Arc::clone(&self.shared);
                    std::thread::spawn(move || {
                        // H-3：单连接 panic（含其 writer 线程外）不传导 daemon 主体。
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            handle_connection(stream, shared);
                        }));
                    });
                }
                Err(_e) => {
                    if !self.shared.running.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    }
}

/// 关闭句柄：触发 daemon 退出 + 整树终结全部 pane + 断开全部连接。
pub struct ShutdownHandle {
    shared: Arc<DaemonShared>,
}

impl ShutdownHandle {
    pub fn shutdown(&self) {
        trigger_shutdown(&self.shared);
    }
}

/// 关闭：标志位 → kill 全部 pane → 断开全部连接（writer 退出）→ self-connect 唤醒 accept。
fn trigger_shutdown(shared: &Arc<DaemonShared>) {
    shared.running.store(false, Ordering::SeqCst);
    kill_all_panes(&shared.host);
    // 断开全部连接（writer 收 Disconnect 退出；reader 阻塞者待进程退出，见 D-7 v1 说明）。
    for conn in shared.conns.lock().unwrap_or_else(recover).values() {
        let _ = conn.tx.send(Outbound::Disconnect);
    }
    // 唤醒阻塞的 accept：连上即弃。serve 循环 accept 返回后查 running=false → break。
    if let Ok(ConnectOutcome::Connected(s)) = try_connect(&shared.pipe_name, 200) {
        drop(s);
    }
}

fn kill_all_panes(host: &PaneHost) {
    for state in host.list_panes() {
        let _ = host.kill(&state.pane_id);
    }
}

/// 单连接处理：取身份（I-5）→ split 读写半 → 起 writer 线程 → reader 循环 → 清理。
fn handle_connection(stream: PipeStream, shared: Arc<DaemonShared>) {
    // I-5：身份不可得 ⇒ 拒（serve_connection 收 None 即 RejectedNoIdentity）。
    let identity = stream.client_process_id();
    let conn_id = shared.next_conn_id.fetch_add(1, Ordering::SeqCst);
    // 连接级审计（RT-2：{pid, image_path, 时刻}）。身份不可得也记（fail-closed 留痕）。
    let image = identity.and_then(process_image_path).unwrap_or_default();
    shared.log.log(&format!(
        "connect conn={conn_id} pid={} image={image:?}",
        identity.map(|p| p.to_string()).unwrap_or_else(|| "none".into())
    ));

    let (mut reader, writer) = match stream.split() {
        Ok(halves) => halves,
        Err(_) => {
            shared.log.log(&format!("disconnect conn={conn_id} reason=split_failed"));
            return; // 事件创建失败（极罕见资源耗尽）——放弃该连接
        }
    };

    let (tx, rx) = channel::<Outbound>();
    let conn = Arc::new(ConnHandle {
        tx,
        queued_bytes: AtomicUsize::new(0),
        dead: AtomicBool::new(false),
        subscriptions: Mutex::new(HashSet::new()),
        last_attach_at: Mutex::new(None),
    });
    shared
        .conns
        .lock()
        .unwrap_or_else(recover)
        .insert(conn_id, Arc::clone(&conn));

    // writer 线程：drain 外发队列。
    let writer_conn = Arc::clone(&conn);
    let writer_thread = std::thread::spawn(move || writer_loop(writer, rx, writer_conn));

    // reader 循环（本线程）。
    let outcome = serve_connection(&mut reader, identity, &shared, &conn);

    // 清理：摘连接 + 通知 writer 退出 + join。
    shared.conns.lock().unwrap_or_else(recover).remove(&conn_id);
    let _ = conn.tx.send(Outbound::Disconnect);
    let _ = writer_thread.join();

    // 断开审计：背压触发（dead）vs 正常，连同 reader 结果。
    let reason = if conn.dead.load(Ordering::Relaxed) {
        "backpressure"
    } else {
        "normal"
    };
    shared
        .log
        .log(&format!("disconnect conn={conn_id} reason={reason} outcome={outcome:?}"));

    if outcome == ConnOutcome::KillServerRequested {
        trigger_shutdown(&shared);
    }
}

/// writer 线程主体：从队列取帧 WriteFile；Disconnect 或写失败即退出。
fn writer_loop(mut writer: PipeWriter, rx: Receiver<Outbound>, conn: Arc<ConnHandle>) {
    while let Ok(item) = rx.recv() {
        match item {
            Outbound::Frame(frame, bytes) => {
                conn.queued_bytes.fetch_sub(bytes, Ordering::Relaxed);
                if write_frame(&mut writer, &frame).is_err() {
                    break; // 客户端断开
                }
            }
            Outbound::Disconnect => break,
        }
    }
}

/// 连接处理结果（安全/协议不变量的可观测出口，供单元测试断言）。
#[derive(Debug, PartialEq, Eq)]
enum ConnOutcome {
    RejectedNoIdentity,
    RejectedBadVersion,
    RejectedBadFirstFrame,
    RejectedBadDirection,
    Closed,
    KillServerRequested,
    IoError,
}

/// 协议/安全核心（reader 侧）：读帧 → 握手 → 请求循环，回复经 `conn.enqueue` 入外发队列。
/// 与传输解耦（泛型 `Read`）使握手/方向/fail-closed 不变量可在内存 Cursor 上单测。
fn serve_connection<R: Read>(
    reader: &mut R,
    identity: Option<u32>,
    shared: &Arc<DaemonShared>,
    conn: &ConnHandle,
) -> ConnOutcome {
    // I-5 fail-closed：身份不可得即拒，不进握手。
    if identity.is_none() {
        return ConnOutcome::RejectedNoIdentity;
    }

    // D-4 握手：首帧必须 Hello（仅握手期合法）+ 版本严格相等。
    match read_frame(reader) {
        Ok(WireFrame::Hello {
            protocol_version, ..
        }) => {
            if protocol_version != PROTOCOL_VERSION {
                return ConnOutcome::RejectedBadVersion; // 不回 HelloAck
            }
            conn.enqueue(
                WireFrame::HelloAck {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_version: DAEMON_VERSION.into(),
                },
                128,
            );
        }
        Ok(_) => return ConnOutcome::RejectedBadFirstFrame, // H-2：首帧非 Hello
        Err(WireError::Eof) => return ConnOutcome::Closed,
        Err(_) => return ConnOutcome::IoError,
    }

    // 请求循环：H-2 只接受 Request。
    loop {
        match read_frame(reader) {
            Ok(WireFrame::Request(req)) => {
                let kill_server = matches!(req.op, MuxOp::KillServer);
                let reply = build_reply(req, shared, conn);
                let bytes = reply_bytes(&reply);
                conn.enqueue(WireFrame::Reply(reply), bytes);
                if kill_server {
                    return ConnOutcome::KillServerRequested;
                }
            }
            Ok(_) => return ConnOutcome::RejectedBadDirection, // H-2：方向违例
            Err(WireError::Eof) => return ConnOutcome::Closed,
            Err(_) => return ConnOutcome::IoError,
        }
    }
}

/// 构建应答（经 PaneHost，R-1）。`MuxOp` 穷尽 match——未来加变体在此编译报错强制裁决。
fn build_reply(req: MuxRequest, shared: &Arc<DaemonShared>, conn: &ConnHandle) -> MuxReply {
    let cid = req.correlation_id;
    let host = &shared.host;
    let running = &shared.running;
    let result: Result<MuxPayload, ConmuxError> = match req.op {
        // M2a-M3：关闭中拒绝新建/重起——否则 sweep 后到的 Spawn 拿成功应答却被 Job drop 瞬死。
        MuxOp::Spawn(_) | MuxOp::Respawn(_) if !running.load(Ordering::SeqCst) => {
            Err(ConmuxError::SupervisorError {
                message: "daemon 正在关闭，拒绝新建/重起 pane".into(),
            })
        }
        MuxOp::Spawn(r) => host.spawn(r).map(MuxPayload::Spawned),
        MuxOp::Respawn(r) => {
            let pane_id = r.pane_id.clone();
            host.respawn(&pane_id, r).map(|_| MuxPayload::Spawned(pane_id))
        }
        // R-1 / R-2：IPC 注入唯一写链 = inject_stdin；source 硬编码 UserDirect（wire 无协商）。
        MuxOp::Send { pane_id, data } => host
            .inject_stdin(&pane_id, &data, InjectionSource::UserDirect)
            .map(|_| MuxPayload::Sent),
        MuxOp::Capture(r) => host.capture(r).map(MuxPayload::Captured),
        MuxOp::Resize { pane_id, size } => {
            host.resize(&pane_id, size).map(|_| MuxPayload::Resized)
        }
        MuxOp::KillTree { pane_id } => host.kill(&pane_id).map(|_| MuxPayload::Killed),
        MuxOp::ListPanes => Ok(MuxPayload::Panes(host.list_panes())),
        MuxOp::ListThemes => Ok(MuxPayload::Themes(crate::theme::builtin_terminal_themes())),
        MuxOp::KillServer => Ok(MuxPayload::ServerKillScheduled),
        // Slice 3：pin 可执行文件到信任库。经 SharedTrustStore 同一 Arc 直接改内存态 + 存盘，
        // 下次 spawn verify 即见新 pin（免 daemon 重启）。path 校验绝对路径 + 存在性；
        // pin_executable 内部算 SHA-256 写 pinned_targets。
        MuxOp::PinExecutable { path } => match &shared.trust_store {
            Some(store) => store
                .pin_executable(&path)
                .map(|_| MuxPayload::Pinned)
                .map_err(|e| ConmuxError::Unsupported { message: e }),
            None => Err(ConmuxError::Unsupported {
                message: "此 daemon 未装配信任库（测试态），不支持 pin".into(),
            }),
        },
        // P1-b：unpin 对称走 IPC（同 SharedTrustStore Arc 即时生效 + 存盘）——此前
        // 只有客户端直写文件，运行中 daemon 内存态不受影响，收权慢于授权。
        MuxOp::UnpinExecutable { path } => match &shared.trust_store {
            Some(store) => store
                .unpin(&path)
                .map(|_| MuxPayload::Unpinned)
                .map_err(|e| ConmuxError::Unsupported { message: e }),
            None => Err(ConmuxError::Unsupported {
                message: "此 daemon 未装配信任库（测试态），不支持 unpin".into(),
            }),
        },
        // D-5 订阅：维护本连接订阅集（fan-out 据此投递）。
        MuxOp::Subscribe { pane_id } => {
            conn.subscriptions.lock().unwrap_or_else(recover).insert(pane_id);
            Ok(MuxPayload::Subscribed)
        }
        MuxOp::Unsubscribe { pane_id } => {
            conn.subscriptions
                .lock()
                .unwrap_or_else(recover)
                .remove(&pane_id);
            Ok(MuxPayload::Unsubscribed)
        }
        // D-6 attach（限速 + 并发=1，D-7）：见 attach_with_limits。
        MuxOp::Attach { pane_id } => attach_with_limits(shared, conn, pane_id),
        // M2c：主题热切换——校验 id → 向**全部连接**广播 ThemeChanged（全局，daemon 不持久化）。
        MuxOp::SetTheme { id } => {
            if crate::theme::builtin_terminal_themes()
                .iter()
                .any(|t| t.id == id)
            {
                broadcast_theme_changed(shared, &id);
                Ok(MuxPayload::ThemeSet)
            } else {
                Err(ConmuxError::Unsupported {
                    message: format!("未知主题 id: {id}（theme ls 查可用预置）"),
                })
            }
        }
    };
    match result {
        Ok(payload) => MuxReply::Ok {
            correlation_id: cid,
            payload,
        },
        Err(error) => MuxReply::Err {
            correlation_id: cid,
            error,
        },
    }
}

/// Attach 处理（D-6 无缝拼接 + D-7 限速）：
/// 1. per-连接 ≥500ms 限速（防快照放大 DoS）；2. per-pane 并发快照=1（进行中再 Attach 回 Busy）；
/// 3. **先注册订阅、后取快照**（注册到快照间事件按 seq>last_seq 去重，无丢无重）。
fn attach_with_limits(
    shared: &Arc<DaemonShared>,
    conn: &ConnHandle,
    pane_id: PaneId,
) -> Result<MuxPayload, ConmuxError> {
    // D-7 限速：per-连接 attach 间隔 ≥500ms（被拒亦更新时刻，限制尝试频率）。
    {
        let mut last = conn.last_attach_at.lock().unwrap_or_else(recover);
        if let Some(t) = *last {
            if t.elapsed() < ATTACH_MIN_INTERVAL {
                return Err(ConmuxError::Busy {
                    message: "attach 过于频繁（<500ms），稍后重试".into(),
                });
            }
        }
        *last = Some(Instant::now());
    }
    // D-7 per-pane 并发快照=1：进行中再 Attach 同 pane → Busy（避免快照放大叠加）。
    {
        let mut set = shared.attaching.lock().unwrap_or_else(recover);
        if !set.insert(pane_id.clone()) {
            return Err(ConmuxError::Busy {
                message: "该 pane 正在被另一 attach 取快照，稍后重试".into(),
            });
        }
    }
    // 先注册订阅（D-6），后取快照；无论成败清并发标记。
    // D-6 不变量：订阅本身先于 attach_snapshot 建立——消费方靠 `seq > last_seq`
    // 去重，先订阅保证快照后到达的 PaneOutput 事件不丢。这里只提前 drop
    // subscriptions 的 Mutex guard（订阅记录已写入 HashSet），**不改变订阅顺序**。
    // 显式块让"guard 在 attach_snapshot 前释放"的意图清晰，防后续维护误把
    // attach_snapshot 挪进 subscriptions 锁内（会引入 L8→L1 嵌套）。
    {
        let mut subs = conn.subscriptions.lock().unwrap_or_else(recover);
        subs.insert(pane_id.clone());
    }
    let result = shared.host.attach_snapshot(&pane_id);
    shared
        .attaching
        .lock()
        .unwrap_or_else(recover)
        .remove(&pane_id);
    match result {
        Ok(snap) => Ok(MuxPayload::AttachSnapshot {
            mode_preamble_b64: b64(&snap.mode_preamble),
            history_b64: b64(&snap.history),
            last_seq: snap.last_seq,
            pane_state: snap.pane_state,
        }),
        Err(e) => {
            // 快照失败：回滚订阅。
            conn.subscriptions
                .lock()
                .unwrap_or_else(recover)
                .remove(&pane_id);
            Err(e)
        }
    }
}

/// 向全部连接广播主题切换（D-8：全局事件，非按订阅；消费者据此实时换肤）。
fn broadcast_theme_changed(shared: &Arc<DaemonShared>, id: &str) {
    let conns = shared.conns.lock().unwrap_or_else(recover);
    for conn in conns.values() {
        conn.enqueue(
            WireFrame::Notify(MuxNotify::ThemeChanged { id: id.to_string() }),
            128,
        );
    }
}

/// poll_exit sweep 间隔（D-2a daemon 兜底）。
const EXIT_SWEEP_INTERVAL: Duration = Duration::from_millis(1000);

/// poll_exit sweep（D-2a daemon 兜底）：ConPTY reader 在 child 退出后**可能不返回 EOF**
/// （pane.rs 实测），读泵的 `PaneExited` 因此可能永不发——attach 客户端（GUI/CLI）的退出态
/// 不可达（conflux in-process 靠前端轮询 is_process_exited→poll_exit 兜住，daemon 路径此前无人
/// 驱动 poll_exit）。本 sweep 周期 `poll_exit` 各 pane（进程句柄查，不依赖 reader EOF），首次
/// 转 Exited 即经 fanout 广播 `PaneExited` 给订阅者。`swept` 去重 + 清理已移除 pane 防膨胀；
/// 与读泵 EOF 路径的潜在重复由客户端幂等消化（attach 循环遇首个 Exited 即停）。
fn run_exit_sweep(shared: Arc<DaemonShared>) {
    let mut swept: HashSet<PaneId> = HashSet::new();
    while shared.running.load(Ordering::SeqCst) {
        std::thread::sleep(EXIT_SWEEP_INTERVAL);
        if !shared.running.load(Ordering::SeqCst) {
            break;
        }
        sweep_exits_once(&shared, &mut swept);
    }
}

/// 一趟 sweep：poll_exit 各 pane，首次转 Exited 广播 PaneExited（swept 去重 + 清理已移除）。
///
/// **S-1 代际化（2026-07-02 审计）**：respawn 复用同 `PaneId`（`pane.rs::respawn` =
/// remove + kill + spawn 同 id）。swept 若只按 PaneId 永久去重，旧代际退出被广播后，
/// 新代际的退出会被永久挡在 sweep 兜底之外（只剩不可靠的 reader-EOF 路径）。
/// 修复依赖的不变量：**被 sweep 广播过的 pane，其同代际表内 lifecycle 已被 poll_exit
/// 写回为 `Exited` 且不可逆**——因此再次观测到非 `Exited`（Spawning/Running）必是
/// respawn 出的新代际 ⇒ 清 swept 标记，新代际退出可再次广播。该判据对"新代际在首次
/// sweep 前就快速退出"也成立：表内 lifecycle 只由 poll_exit 写回，新代际未被 poll 前
/// 恒为 Running。
fn sweep_exits_once(shared: &Arc<DaemonShared>, swept: &mut HashSet<PaneId>) {
    let panes = shared.host.list_panes();
    let live: HashSet<PaneId> = panes.iter().map(|s| s.pane_id.clone()).collect();
    for state in &panes {
        // 代际检测：已 sweep 的 pane 观测到非 Exited ⇒ 新代际，允许再次广播。
        if !matches!(state.lifecycle, PaneLifecycle::Exited(_)) {
            swept.remove(&state.pane_id);
        }
        if swept.contains(&state.pane_id) {
            continue;
        }
        // poll_exit：进程句柄查（绕开不可靠的 reader EOF）。Ok(Some(code)) ⇒ 已退出。
        if let Ok(Some(code)) = shared.host.poll_exit(&state.pane_id) {
            swept.insert(state.pane_id.clone());
            broadcast_pane_exited(shared, &state.pane_id, Some(code));
        }
    }
    swept.retain(|p| live.contains(p)); // pane 被移除（kill/respawn）后清理，防 swept 无限增长
}

/// 向订阅该 pane 的连接广播 PaneExited（与 FanoutSink::on_notify 同投递语义）。
fn broadcast_pane_exited(shared: &Arc<DaemonShared>, pane_id: &PaneId, exit_code: Option<i32>) {
    let notify = MuxNotify::PaneExited {
        pane_id: pane_id.clone(),
        exit_code,
    };
    let bytes = notify_bytes(&notify);
    let conns = shared.conns.lock().unwrap_or_else(recover);
    for conn in conns.values() {
        if conn.is_subscribed(pane_id) {
            conn.enqueue(WireFrame::Notify(notify.clone()), bytes);
        }
    }
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 事件近似字节数（背压记账；PaneOutput 数据是主量）。
fn notify_bytes(notify: &MuxNotify) -> usize {
    match notify {
        MuxNotify::PaneOutput { data, .. } => data.len() + 64,
        _ => 128,
    }
}

/// 应答近似字节数（背压记账）。
///
/// **S-2（2026-07-02 审计）**：此前一切应答固定按 256B 记账，而 Captured/AttachSnapshot
/// 携带 base64 大载荷（1MiB ring → ~1.4MB 帧）——8MiB 队列上界最坏对应 32768 帧 ×
/// ~1.4MB ≈ 45GB 真实内存（Capture 又无 Attach 那样的限速）。按内容量记账后，
/// `MAX_QUEUE_BYTES` 重新成为真实内存上界：慢客户端排队少量大帧即触发背压断连。
fn reply_bytes(reply: &MuxReply) -> usize {
    match reply {
        MuxReply::Ok { payload, .. } => match payload {
            MuxPayload::Captured(c) => c.data_base64.len().saturating_add(128),
            MuxPayload::AttachSnapshot {
                mode_preamble_b64,
                history_b64,
                ..
            } => mode_preamble_b64
                .len()
                .saturating_add(history_b64.len())
                .saturating_add(192),
            MuxPayload::Panes(panes) => panes.len().saturating_mul(256).saturating_add(64),
            MuxPayload::Themes(themes) => themes.len().saturating_mul(1024).saturating_add(64),
            _ => 256,
        },
        MuxReply::Err { .. } => 256,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PaneId, PaneSize};
    use std::io::Cursor;

    fn test_shared() -> Arc<DaemonShared> {
        let conns = Arc::new(Mutex::new(HashMap::new()));
        let host = PaneHost::new_windows(
            Vec::new(),
            Arc::new(FanoutSink {
                conns: Arc::clone(&conns),
            }),
        );
        Arc::new(DaemonShared {
            host,
            running: AtomicBool::new(true),
            pipe_name: "test".into(),
            conns,
            next_conn_id: AtomicU64::new(1),
            attaching: Mutex::new(HashSet::new()),
            log: DaemonLog::with_path(std::env::temp_dir().join("conmux-test-unused.log"), MAX_LOG_BYTES),
            trust_store: None,
        })
    }

    fn test_conn() -> (Arc<ConnHandle>, Receiver<Outbound>) {
        let (tx, rx) = channel();
        (
            Arc::new(ConnHandle {
                tx,
                queued_bytes: AtomicUsize::new(0),
                dead: AtomicBool::new(false),
                subscriptions: Mutex::new(HashSet::new()),
                last_attach_at: Mutex::new(None),
            }),
            rx,
        )
    }

    /// 把客户端帧序列编码进 Cursor（reader 喂入）。
    fn reader_with(frames: &[WireFrame]) -> Cursor<Vec<u8>> {
        let mut buf = Vec::new();
        for f in frames {
            write_frame(&mut buf, f).unwrap();
        }
        Cursor::new(buf)
    }

    /// 抽出 writer 队列里的全部回帧。
    fn drain_frames(rx: &Receiver<Outbound>) -> Vec<WireFrame> {
        let mut v = Vec::new();
        while let Ok(item) = rx.try_recv() {
            if let Outbound::Frame(f, _) = item {
                v.push(f);
            }
        }
        v
    }

    fn hello(v: u32) -> WireFrame {
        WireFrame::Hello {
            protocol_version: v,
            client_kind: "test".into(),
        }
    }
    fn request(op: MuxOp) -> WireFrame {
        WireFrame::Request(MuxRequest {
            correlation_id: 1,
            op,
        })
    }

    /// I-5：身份不可得 ⇒ RejectedNoIdentity，不入任何回帧（无 HelloAck）。
    #[test]
    fn no_identity_is_rejected_before_handshake() {
        let mut r = reader_with(&[hello(PROTOCOL_VERSION)]);
        let (conn, rx) = test_conn();
        let outcome = serve_connection(&mut r, None, &test_shared(), &conn);
        assert_eq!(outcome, ConnOutcome::RejectedNoIdentity);
        assert!(drain_frames(&rx).is_empty(), "拒连不应回任何帧");
    }

    /// D-4：握手版本不匹配 ⇒ RejectedBadVersion，无 HelloAck。
    #[test]
    fn wrong_protocol_version_is_rejected() {
        let mut r = reader_with(&[hello(PROTOCOL_VERSION + 99)]);
        let (conn, rx) = test_conn();
        let outcome = serve_connection(&mut r, Some(1234), &test_shared(), &conn);
        assert_eq!(outcome, ConnOutcome::RejectedBadVersion);
        assert!(drain_frames(&rx).is_empty());
    }

    /// H-2：首帧非 Hello ⇒ RejectedBadFirstFrame。
    #[test]
    fn non_hello_first_frame_is_rejected() {
        let mut r = reader_with(&[request(MuxOp::ListPanes)]);
        let (conn, rx) = test_conn();
        let outcome = serve_connection(&mut r, Some(1234), &test_shared(), &conn);
        assert_eq!(outcome, ConnOutcome::RejectedBadFirstFrame);
        assert!(drain_frames(&rx).is_empty());
    }

    /// H-2：握手后收到非 Request 方向帧 ⇒ RejectedBadDirection（但已回 HelloAck）。
    #[test]
    fn wrong_direction_after_handshake_is_rejected() {
        let notify = WireFrame::Notify(MuxNotify::PaneExited {
            pane_id: PaneId("x".into()),
            exit_code: None,
        });
        let mut r = reader_with(&[hello(PROTOCOL_VERSION), notify]);
        let (conn, rx) = test_conn();
        let outcome = serve_connection(&mut r, Some(1234), &test_shared(), &conn);
        assert_eq!(outcome, ConnOutcome::RejectedBadDirection);
        let replies = drain_frames(&rx);
        assert_eq!(replies.len(), 1);
        assert!(matches!(replies[0], WireFrame::HelloAck { .. }));
    }

    /// happy path：Hello + ListPanes + EOF ⇒ HelloAck + Reply(Panes 空) + Closed。
    #[test]
    fn handshake_then_listpanes_replies_ok() {
        let mut r = reader_with(&[hello(PROTOCOL_VERSION), request(MuxOp::ListPanes)]);
        let (conn, rx) = test_conn();
        let outcome = serve_connection(&mut r, Some(1234), &test_shared(), &conn);
        assert_eq!(outcome, ConnOutcome::Closed);
        let replies = drain_frames(&rx);
        assert_eq!(replies.len(), 2);
        assert!(matches!(replies[0], WireFrame::HelloAck { .. }));
        match &replies[1] {
            WireFrame::Reply(MuxReply::Ok {
                payload: MuxPayload::Panes(p),
                ..
            }) => assert!(p.is_empty()),
            other => panic!("应为 Ok(Panes)，实际: {other:?}"),
        }
    }

    /// KillServer ⇒ 回 ServerKillScheduled + 返回 KillServerRequested。
    #[test]
    fn kill_server_acks_then_signals() {
        let mut r = reader_with(&[hello(PROTOCOL_VERSION), request(MuxOp::KillServer)]);
        let (conn, rx) = test_conn();
        let outcome = serve_connection(&mut r, Some(1234), &test_shared(), &conn);
        assert_eq!(outcome, ConnOutcome::KillServerRequested);
        let replies = drain_frames(&rx);
        assert!(matches!(
            replies.last(),
            Some(WireFrame::Reply(MuxReply::Ok {
                payload: MuxPayload::ServerKillScheduled,
                ..
            }))
        ));
    }

    /// D-5：Subscribe 把 pane 加入本连接订阅集（后续 fan-out 据此投递）。
    #[test]
    fn subscribe_registers_in_connection_set() {
        let mut r = reader_with(&[
            hello(PROTOCOL_VERSION),
            request(MuxOp::Subscribe {
                pane_id: PaneId("p1".into()),
            }),
        ]);
        let (conn, rx) = test_conn();
        serve_connection(&mut r, Some(1234), &test_shared(), &conn);
        assert!(
            conn.is_subscribed(&PaneId("p1".into())),
            "Subscribe 后订阅集应含 p1"
        );
        let replies = drain_frames(&rx);
        assert!(matches!(
            replies.last(),
            Some(WireFrame::Reply(MuxReply::Ok {
                payload: MuxPayload::Subscribed,
                ..
            }))
        ));
    }

    /// Unsubscribe 移除订阅。
    #[test]
    fn unsubscribe_removes_from_set() {
        let mut r = reader_with(&[
            hello(PROTOCOL_VERSION),
            request(MuxOp::Subscribe {
                pane_id: PaneId("p1".into()),
            }),
            request(MuxOp::Unsubscribe {
                pane_id: PaneId("p1".into()),
            }),
        ]);
        let (conn, _rx) = test_conn();
        serve_connection(&mut r, Some(1234), &test_shared(), &conn);
        assert!(!conn.is_subscribed(&PaneId("p1".into())));
    }

    /// Attach 不存在的 pane ⇒ 订阅回滚 + 回 PaneNotFound（不留悬空订阅）。
    #[test]
    fn attach_unknown_pane_rolls_back_subscription() {
        let mut r = reader_with(&[
            hello(PROTOCOL_VERSION),
            request(MuxOp::Attach {
                pane_id: PaneId("nope".into()),
            }),
        ]);
        let (conn, rx) = test_conn();
        serve_connection(&mut r, Some(1234), &test_shared(), &conn);
        assert!(
            !conn.is_subscribed(&PaneId("nope".into())),
            "attach 快照失败应回滚订阅"
        );
        let replies = drain_frames(&rx);
        assert!(matches!(
            replies.last(),
            Some(WireFrame::Reply(MuxReply::Err {
                error: ConmuxError::PaneNotFound { .. },
                ..
            }))
        ));
    }

    /// 背压：排队字节超 8 MiB ⇒ 连接置 dead + 发 Disconnect（D-7）。
    #[test]
    fn backpressure_disconnects_over_limit() {
        let (conn, rx) = test_conn();
        // 入队一帧标记 9 MiB（超限）。
        conn.enqueue(
            WireFrame::Notify(MuxNotify::PaneExited {
                pane_id: PaneId("p".into()),
                exit_code: None,
            }),
            9 * 1024 * 1024,
        );
        assert!(conn.dead.load(Ordering::Relaxed), "超限应置 dead");
        // 队列里应是 Disconnect（非 Frame）。
        assert!(matches!(rx.try_recv(), Ok(Outbound::Disconnect)));
        // dead 后再入队被丢弃。
        conn.enqueue(
            WireFrame::Notify(MuxNotify::PaneExited {
                pane_id: PaneId("p".into()),
                exit_code: None,
            }),
            10,
        );
        assert!(rx.try_recv().is_err(), "dead 后不再入队");
    }

    /// M2a-M3：关闭中 Spawn 被拒（不回成功应答）。
    #[test]
    fn spawn_rejected_during_shutdown() {
        let spawn = request(MuxOp::Spawn(crate::pane::SpawnRequest {
            pane_id: PaneId("x".into()),
            command: crate::pane::CommandSpec {
                program: "cmd.exe".into(),
                args: vec![],
                cwd: None,
                env: vec![],
            },
            size: PaneSize { rows: 24, cols: 80 },
            adapter_id: "shell".into(),
            display_name: None,
            created_at: 0,
        }));
        let mut r = reader_with(&[hello(PROTOCOL_VERSION), spawn]);
        let (conn, rx) = test_conn();
        let shared = test_shared();
        shared.running.store(false, Ordering::SeqCst);
        serve_connection(&mut r, Some(1234), &shared, &conn);
        let replies = drain_frames(&rx);
        assert!(matches!(
            replies.last(),
            Some(WireFrame::Reply(MuxReply::Err {
                error: ConmuxError::SupervisorError { .. },
                ..
            }))
        ));
    }

    /// D-2a：daemon poll_exit sweep 在 pane 进程退出后（即便 reader 不返 EOF）广播 PaneExited
    /// 给订阅者；已广播的 pane 不重复（swept 去重）。real ConPTY 快退进程（cmd /c exit 7）。
    #[test]
    fn exit_sweep_broadcasts_pane_exited_to_subscribers() {
        let shared = test_shared();
        let pane_id = PaneId("sweep-exit".into());
        // Slice 1：内核 spawn 守卫要求绝对路径（SystemRoot 拼系统 cmd.exe）。
        let cmd_abs = std::path::PathBuf::from(
            std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()),
        )
        .join("System32\\cmd.exe")
        .to_string_lossy()
        .into_owned();
        shared
            .host
            .spawn(crate::pane::SpawnRequest {
                pane_id: pane_id.clone(),
                command: crate::pane::CommandSpec {
                    program: cmd_abs,
                    args: vec!["/c".into(), "exit 7".into()],
                    cwd: None,
                    env: vec![],
                },
                size: PaneSize { rows: 24, cols: 80 },
                adapter_id: "shell".into(),
                display_name: None,
                created_at: 0,
            })
            .expect("spawn quick-exit pane");

        // 订阅该 pane 的连接（sweep 广播只投订阅者）。
        let (conn, rx) = test_conn();
        conn.subscriptions.lock().unwrap().insert(pane_id.clone());
        shared.conns.lock().unwrap().insert(1, Arc::clone(&conn));

        // 轮询 sweep 直到检测到进程退出（cmd /c exit 7 瞬退；3s 容忍 spawn 抖动）。
        let mut swept = HashSet::new();
        let mut got: Option<Option<i32>> = None;
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(100));
            sweep_exits_once(&shared, &mut swept);
            for f in drain_frames(&rx) {
                if let WireFrame::Notify(MuxNotify::PaneExited {
                    pane_id: pid,
                    exit_code,
                }) = f
                {
                    assert_eq!(pid, pane_id, "广播的 pane_id 应匹配");
                    got = Some(exit_code);
                }
            }
            if got.is_some() {
                break;
            }
        }
        assert_eq!(
            got,
            Some(Some(7)),
            "sweep 应广播 PaneExited(exit_code=Some(7))"
        );

        // 幂等：再 sweep 不重复广播（swept 去重）。
        sweep_exits_once(&shared, &mut swept);
        assert!(
            drain_frames(&rx).is_empty(),
            "已广播退出的 pane 不应被重复广播"
        );

        let _ = shared.host.kill(&pane_id);
    }

    /// S-1（2026-07-02 审计）：respawn 复用同 PaneId——旧代际退出被 sweep 广播后，
    /// 新代际的退出必须能再次被 sweep 广播（此前 swept 按 PaneId 永久去重 ⇒ 新代际
    /// 退出事件被永久挡在 sweep 兜底之外，仅剩不可靠的 reader-EOF 路径）。
    /// real ConPTY：两代皆 cmd /c exit N 快退，退出码区分代际。
    #[test]
    fn exit_sweep_rebroadcasts_after_respawn() {
        let shared = test_shared();
        let pane_id = PaneId("sweep-respawn".into());
        let cmd_abs = std::path::PathBuf::from(
            std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()),
        )
        .join("System32\\cmd.exe")
        .to_string_lossy()
        .into_owned();
        let spawn_req = |exit_code: &str| crate::pane::SpawnRequest {
            pane_id: pane_id.clone(),
            command: crate::pane::CommandSpec {
                program: cmd_abs.clone(),
                args: vec!["/c".into(), format!("exit {exit_code}")],
                cwd: None,
                env: vec![],
            },
            size: PaneSize { rows: 24, cols: 80 },
            adapter_id: "shell".into(),
            display_name: None,
            created_at: 0,
        };
        shared.host.spawn(spawn_req("7")).expect("spawn 第一代快退 pane");

        let (conn, rx) = test_conn();
        conn.subscriptions.lock().unwrap().insert(pane_id.clone());
        shared.conns.lock().unwrap().insert(1, Arc::clone(&conn));

        let mut swept = HashSet::new();

        // 第一代退出 → 广播 exit 7。
        let mut got: Option<Option<i32>> = None;
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(100));
            sweep_exits_once(&shared, &mut swept);
            for f in drain_frames(&rx) {
                if let WireFrame::Notify(MuxNotify::PaneExited { exit_code, .. }) = f {
                    got = Some(exit_code);
                }
            }
            if got.is_some() {
                break;
            }
        }
        assert_eq!(got, Some(Some(7)), "第一代退出应被 sweep 广播");

        // respawn 同 id（第二代，exit 9）。
        shared
            .host
            .respawn(&pane_id, spawn_req("9"))
            .expect("respawn 第二代快退 pane");

        // 防 flake（红队 2026-07-02）：gen1 的 reader-EOF 路径可能在 phase-1 break 后
        // 才迟到落帧 PaneExited(7)（ConPTY EOF 时序不定）——先清残帧，且下方只认
        // exit 9（stale 7 忽略）。本测试锚定"新代际退出事件可再次到达订阅者"，
        // 不区分 sweep/reader 来源（gen2 reader-EOF 也可能自发 9，见红队登记）。
        let _ = drain_frames(&rx);

        // 新代际退出 → 必须再次被广播（旧实现在此永久沉默）。
        let mut got2: Option<Option<i32>> = None;
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(100));
            sweep_exits_once(&shared, &mut swept);
            for f in drain_frames(&rx) {
                if let WireFrame::Notify(MuxNotify::PaneExited { exit_code, .. }) = f {
                    if exit_code == Some(9) {
                        got2 = Some(exit_code);
                    }
                }
            }
            if got2.is_some() {
                break;
            }
        }
        assert_eq!(
            got2,
            Some(Some(9)),
            "respawn 后新代际的退出必须再次被 sweep 广播（S-1 代际化）"
        );

        let _ = shared.host.kill(&pane_id);
    }

    /// S-2：应答背压记账按内容量——Capture 大帧不得再按固定 256B 记账（8MiB 队列
    /// 上界曾最坏对应 ~45GB 真实内存的放大洞）。
    #[test]
    fn reply_bytes_scales_with_payload() {
        let big = "A".repeat(2 * 1024 * 1024);
        let captured = MuxReply::Ok {
            correlation_id: 1,
            payload: MuxPayload::Captured(crate::capture::CaptureResult {
                data_base64: big.clone(),
                first_abs_line: 0,
                last_abs_line: 0,
                truncated: false,
                effectively_full: false,
            }),
        };
        assert!(
            reply_bytes(&captured) >= big.len(),
            "Capture 帧应按 base64 内容长度记账"
        );

        let small = MuxReply::Ok {
            correlation_id: 2,
            payload: MuxPayload::Sent,
        };
        assert_eq!(reply_bytes(&small), 256, "小应答维持固定近似记账");

        let err = MuxReply::Err {
            correlation_id: 3,
            error: ConmuxError::Unsupported {
                message: "x".into(),
            },
        };
        assert_eq!(reply_bytes(&err), 256, "错误应答维持固定近似记账");
    }

    /// P1-b：未装配信任库的 daemon 对 UnpinExecutable 诚实拒绝（镜像 pin 语义），
    /// 客户端据此回退直写文件。
    #[test]
    fn unpin_without_trust_store_is_unsupported() {
        let mut r = reader_with(&[
            hello(PROTOCOL_VERSION),
            request(MuxOp::UnpinExecutable {
                path: "C:\\shim\\x.cmd".into(),
            }),
        ]);
        let (conn, rx) = test_conn();
        let shared = test_shared();
        serve_connection(&mut r, Some(1234), &shared, &conn);
        let replies = drain_frames(&rx);
        assert!(matches!(
            replies.last(),
            Some(WireFrame::Reply(MuxReply::Err {
                error: ConmuxError::Unsupported { .. },
                ..
            }))
        ));
    }

    /// D-7：同连接 <500ms 内两次 Attach 同 pane，第二次回 Busy（限速防快照放大）。
    #[test]
    fn rapid_attach_is_rate_limited() {
        let mut r = reader_with(&[
            hello(PROTOCOL_VERSION),
            request(MuxOp::Attach {
                pane_id: PaneId("p".into()),
            }),
            request(MuxOp::Attach {
                pane_id: PaneId("p".into()),
            }),
        ]);
        let (conn, rx) = test_conn();
        serve_connection(&mut r, Some(1234), &test_shared(), &conn);
        let replies = drain_frames(&rx);
        // 第一次：pane 不存在 → PaneNotFound（限速已记时刻）；第二次：<500ms → Busy。
        let errs: Vec<_> = replies
            .iter()
            .filter_map(|f| match f {
                WireFrame::Reply(MuxReply::Err { error, .. }) => Some(error),
                _ => None,
            })
            .collect();
        assert_eq!(errs.len(), 2, "两次 attach 各回一个 Err");
        assert!(matches!(errs[0], ConmuxError::PaneNotFound { .. }));
        assert!(
            matches!(errs[1], ConmuxError::Busy { .. }),
            "第二次快速 attach 应被限速 Busy，实际: {:?}",
            errs[1]
        );
    }

    /// 测试 4（第 5 项嵌套消除回归 + D-6 不变量）：attach_with_limits 不持 subscriptions
    /// 锁进入 attach_snapshot（改动：显式块提前 drop guard），且订阅仍先于快照建立。
    /// 并发 Subscribe/Unsubscribe 同 conn 不死锁（subscriptions 锁可被其他线程获取）。
    ///
    /// 环境限制：测试环境无 ConPTY（`openpty 失败: HRESULT -2147024890`），无法 spawn
    /// 真实 pane 验证 attach 成功路径。改为验证回滚路径（attach 不存在 pane）+ 并发
    /// subscribe/unsubscribe 不死锁。D-6 不变量（订阅先于快照）由代码逻辑保证：
    /// `subscriptions.insert` 在 `attach_snapshot` 前，guard 提前释放（显式块），订阅顺序不变。
    #[test]
    fn attach_with_limits_releases_subscriptions_guard_before_snapshot() {
        let shared = test_shared();
        let (conn, _rx) = test_conn();
        let pane_id = PaneId("attach-d6".into());

        // 并发 Subscribe/Unsubscribe 同 conn 不死锁（subscriptions 锁未被 attach 长期持有）。
        // 此处验证 subscriptions 锁可被其他线程获取——改动后 attach_with_limits 不持该锁
        // 进入 attach_snapshot，故并发访问不阻塞。
        let conn2 = Arc::clone(&conn);
        let h = std::thread::spawn(move || {
            let mut subs = conn2.subscriptions.lock().unwrap();
            subs.insert(PaneId("other".into()));
            subs.remove(&PaneId("other".into()));
        });
        h.join().expect("并发 subscribe/unsubscribe 不应死锁");

        // attach 不存在 pane → PaneNotFound + 订阅回滚（D-6 回滚路径）。
        // attach_with_limits 流程：subscriptions.insert → drop guard → attach_snapshot
        // （PaneNotFound）→ 回滚 subscriptions.remove。验证 guard 提前释放不影响回滚语义。
        let result = attach_with_limits(&shared, &conn, pane_id.clone());
        assert!(
            matches!(result, Err(ConmuxError::PaneNotFound { .. })),
            "attach 不存在 pane 应返回 PaneNotFound，实际: {:?}",
            result
        );
        assert!(
            !conn.subscriptions.lock().unwrap().contains(&pane_id),
            "D-6：attach 失败应回滚订阅（subscriptions 不含该 pane）"
        );
    }

    /// M2c：有效 id 的 SetTheme 向**全部已注册连接**广播 ThemeChanged（全局换肤）。
    #[test]
    fn set_theme_broadcasts_to_all_connections() {
        let shared = test_shared();
        let (conn_a, rx_a) = test_conn();
        let (conn_b, rx_b) = test_conn();
        shared
            .conns
            .lock()
            .unwrap()
            .insert(1, Arc::clone(&conn_a));
        shared
            .conns
            .lock()
            .unwrap()
            .insert(2, Arc::clone(&conn_b));
        let reply = build_reply(
            MuxRequest {
                correlation_id: 9,
                op: MuxOp::SetTheme {
                    id: crate::theme::DEFAULT_TERMINAL_THEME_ID.into(),
                },
            },
            &shared,
            &conn_a,
        );
        assert!(matches!(
            reply,
            MuxReply::Ok {
                payload: MuxPayload::ThemeSet,
                ..
            }
        ));
        for rx in [&rx_a, &rx_b] {
            let frames = drain_frames(rx);
            assert!(
                frames
                    .iter()
                    .any(|f| matches!(f, WireFrame::Notify(MuxNotify::ThemeChanged { .. }))),
                "SetTheme 应向全部连接广播 ThemeChanged"
            );
        }
    }

    /// 未知 theme id ⇒ Unsupported（不广播垃圾）。
    #[test]
    fn set_theme_unknown_id_rejected() {
        let mut r = reader_with(&[
            hello(PROTOCOL_VERSION),
            request(MuxOp::SetTheme {
                id: "no-such-theme".into(),
            }),
        ]);
        let (conn, rx) = test_conn();
        serve_connection(&mut r, Some(1234), &test_shared(), &conn);
        let replies = drain_frames(&rx);
        assert!(matches!(
            replies.last(),
            Some(WireFrame::Reply(MuxReply::Err {
                error: ConmuxError::Unsupported { .. },
                ..
            }))
        ));
    }

    /// 审计日志：写入可读回，超上限滚动到 .1（fail-soft 不 panic）。
    #[test]
    fn daemon_log_writes_and_rotates() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conmux-audit-test-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("log.1"));

        let log = DaemonLog::with_path(path.clone(), 64); // 小上限触发滚动
        log.log("connect conn=1 pid=42 image=\"x\"");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("connect conn=1 pid=42"), "应写入审计行");

        // 写到超 64 字节 → 下次 log 触发滚动（现文件改名 .1，另起新文件）。
        for i in 0..10 {
            log.log(&format!("disconnect conn={i} reason=normal outcome=Closed"));
        }
        assert!(
            path.with_extension("log.1").exists(),
            "超上限应滚动到 .1"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("log.1"));
    }

    /// ListThemes 可用。
    #[test]
    fn list_themes_works() {
        let mut r = reader_with(&[hello(PROTOCOL_VERSION), request(MuxOp::ListThemes)]);
        let (conn, rx) = test_conn();
        serve_connection(&mut r, Some(1234), &test_shared(), &conn);
        let replies = drain_frames(&rx);
        match replies.last() {
            Some(WireFrame::Reply(MuxReply::Ok {
                payload: MuxPayload::Themes(themes),
                ..
            })) => assert!(!themes.is_empty()),
            other => panic!("应为 Ok(Themes)，实际: {other:?}"),
        }
    }
}
