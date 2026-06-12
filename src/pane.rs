//! Pane 抽象与 PaneHost 门面（API 契约 §2 / MF-1/4/6）。
//!
//! 机制层核心：`PaneHost` 是对外门面（spawn/kill/respawn/resize/inject/list）；
//! `PaneBackend`/`PaneSession` 是**内部** trait（`pub(crate)`，不导出），由 `Pane`
//! 私有持有——**模块外无法拿到可写 PTY 句柄**，这把 MF-1「唯一注入路径」做成类型级密封。
//!
//! 唯一写链（冻结）：`InjectionHook 链 → PaneHost::inject_stdin → session.write_all`。
//! 三环之外无写：`PaneSession` 无 `writer()` getter、`Box<dyn Write>` 不出现在任何签名。
//!
//! 本子步（cutover 2a）以 mock backend 立起**注入/生命周期**机制不变量；真实 Windows
//! 后端（portable-pty 0.9 + JobObjectSupervisor + 读线程 + capture）在系统集成子步（2b）落地。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::event::{MuxNotify, PaneEventSink};
use crate::inject::{InjectionContext, InjectionHook};
use crate::job::{ProcessSupervisor, SupervisorFactory};
use crate::scrollback::{LineIndexedBuffer, DEFAULT_BUFFER_CAPACITY};
use crate::types::{InjectionSource, PaneId, PaneLifecycle, PaneSize, PaneState, ScrollbackInfo};
use crate::ConmuxError;

/// 进程启动规格（契约 §13 空白-1 裁决：spawn cwd 用 `cwd`，与 `PaneState.working_dir`
/// 展示语义区分）。retrofit 自 conflux `pty/manager.rs` 的 `CommandBuilder`。
/// serde：经 `MuxOp::Spawn` 过协议面（§7，V2 命名管道预留）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    /// 进程实际 cwd（spawn 入参）；None = 继承当前目录。
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
}

/// spawn 请求。`pane_id` 由调用方提供（= conflux instance_id，契约 §1 不改 ID 体系）——
/// conmux 不生成 ID，避免引入 uuid 依赖且对齐"PaneId == InstanceId"。
/// serde：经 `MuxOp::Spawn` 过协议面（§7）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpawnRequest {
    pub pane_id: PaneId,
    pub command: CommandSpec,
    pub size: PaneSize,
    pub adapter_id: String,
    pub display_name: Option<String>,
    /// 创建时间（Unix ms，调用方提供以保持可测/可重放确定性）。
    pub created_at: i64,
}

// ===== 内部 trait（pub(crate)，不导出——MF-1 隐私墙）=====

/// 后端工厂：打开一个未 spawn 的 PTY 会话。
pub(crate) trait PaneBackend: Send + Sync {
    fn open(&self, size: PaneSize) -> Result<Box<dyn PaneSession>, ConmuxError>;
}

/// 单个 PTY 会话。由 `Pane` 私有持有，模块外不可达。
///
/// **MF-1 不变量**：无任何返回可写句柄的方法（无 `writer()`）；唯一写方法 `write_all`
/// 仅经 `PaneHost::inject_stdin` 调用。`take_reader` 一次性移交、二次返回 Err。
pub(crate) trait PaneSession: Send {
    fn spawn(&mut self, cmd: &CommandSpec) -> Result<u32 /*pid*/, ConmuxError>;
    /// 一次性移交读端：仅 spawn 后由 PaneHost 调一次、立即交读线程；二次调用返回 Err。
    /// （读线程接线在系统集成子步 2b；此前仅 mock 测试覆盖一次性语义。）
    #[allow(dead_code)]
    fn take_reader(&mut self) -> Result<Box<dyn std::io::Read + Send>, ConmuxError>;
    fn resize(&self, size: PaneSize) -> Result<(), ConmuxError>;
    /// 唯一**注入**写方法（MF-1，agent 输入）。trait 对象被 Pane 私有持有，模块外不可达。
    fn write_all(&mut self, data: &[u8]) -> Result<(), ConmuxError>;

    /// 读线程用的**协议回复** writer（DSR `ESC[6n` 应答等机制层回复，**非 agent 注入**）。
    /// 由 PaneHost 读线程在 conmux 内部使用，不导出、调用方不可达，故不违反 MF-1
    /// （MF-1 防的是调用方绕过注入审计，DSR 应答是终端协议回复而非 agent 输入）。
    /// 无 PTY 的后端（mock）返回 None；spawn 之前返回 None。默认 None。
    // 2b-3 PaneHost 读线程接线前 lib build 无调用者，暂为 dead。
    #[allow(dead_code)]
    fn protocol_writer(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>> {
        None
    }

    /// 非阻塞查询子进程退出码（cutover ③ D-2a）。`None` = 仍在运行或不可得。
    /// `PaneHost::poll_exit` 与读线程 EOF 时兑现精确退出码用——ConPTY reader 在 child
    /// 退出后**可能不返回 EOF**（实测已知），消费方必须有事件之外的轮询兜底。
    fn try_exit_code(&mut self) -> Option<i32> {
        None
    }

    /// best-effort 终结已 spawn 的子进程（MF-4 cl.2 / 红队 MF-A）。
    ///
    /// 仅在 `PaneHost::spawn` 的 **supervisor.assign 失败** 分支调用——此时进程已 spawn
    /// 但 JobObject 未接管，drop session 关 ConPTY 不保证杀掉已逃逸的孙进程（spike #4
    /// 实证孤儿孙进程拖死 ClosePseudoConsole）。无监管进程必须主动杀，否则违反
    /// 「assign 失败不产生无监管 pane」。默认 no-op（mock / 无 PTY 后端）；
    /// 成败不可信（portable-pty 0.9 kill 判断写反，spike #5）——纯 best-effort。
    fn kill_best_effort(&mut self) {}
}

/// 单个 pane 的运行时状态（`pub(crate)`，私有字段——不导出本体，仅经 `PaneState` 暴露语义）。
///
/// **D-1a 结构**：`session` 置于 `Arc<Mutex>`——`inject_stdin` 在**全局表锁外**经此句柄写
/// （钩子可执行任意消费方逻辑如审计落库，在表锁内调钩子会制造跨锁死锁窗口）。该 Arc 仅在
/// conmux 内部流转（Pane 本身 `pub(crate)` 私有字段），可写句柄仍不出模块（MF-1 不破）。
/// `inject_lock` 为 per-pane 注入串行锁：保证「before 钩子 → write → after 钩子」整序原子
/// （MF-6），且审计落库顺序 == 字节抵达 PTY 顺序。
pub(crate) struct Pane {
    session: Arc<Mutex<Box<dyn PaneSession>>>,
    inject_lock: Arc<Mutex<()>>,
    supervisor: Box<dyn ProcessSupervisor>,
    lifecycle: PaneLifecycle,
    pid: Option<u32>,
    exit_code: Option<i32>,
    adapter_id: String,
    display_name: Option<String>,
    working_dir: String,
    size: PaneSize,
    created_at: i64,
    /// 行索引 scrollback（读线程 feed；capture / jump-back 后端地基）。
    scrollback: Arc<Mutex<LineIndexedBuffer>>,
}

impl Pane {
    fn to_state(&self, pane_id: &PaneId) -> PaneState {
        let (first, last) = {
            let sb = self.scrollback.lock().expect("scrollback 锁未中毒");
            sb.line_range_available()
        };
        PaneState {
            pane_id: pane_id.clone(),
            adapter_id: self.adapter_id.clone(),
            display_name: self.display_name.clone(),
            lifecycle: self.lifecycle.clone(),
            pid: self.pid,
            exit_code: self.exit_code,
            working_dir: self.working_dir.clone(),
            size: self.size,
            scrollback: ScrollbackInfo {
                total_bytes: self
                    .scrollback
                    .lock()
                    .expect("scrollback 锁未中毒")
                    .total_bytes(),
                first_abs_line: first,
                last_abs_line: last,
            },
            created_at: self.created_at,
        }
    }
}

/// PaneHost 构造配置：后端工厂、监管器工厂、注入钩子链。
///
/// **`pub(crate)`——backend/supervisor 是 conmux 内部（Windows ConPTY / JobObject），
/// conflux 不提供它们**。2a 仅 mock 测试经此构造；2b 将加公开 `PaneHost::new_windows(
/// hooks, event_sink, runtime)` 内部装配 Windows 后端 + JobObjectSupervisor。
// backend/supervisor 是 conmux 内部（Windows ConPTY / JobObject），conflux 不提供。
pub(crate) struct PaneHostConfig {
    pub(crate) backend: Box<dyn PaneBackend>,
    pub(crate) supervisor_factory: Box<dyn SupervisorFactory>,
    pub(crate) hooks: Vec<Arc<dyn InjectionHook>>,
    /// 事件出口（None = 不起读线程，2a mock 测试用；Windows 路径必给）。
    pub(crate) event_sink: Option<Arc<dyn PaneEventSink>>,
}

/// 对外门面。私有持有 pane 表；唯一写入口 `inject_stdin`（MF-1）。
pub struct PaneHost {
    backend: Box<dyn PaneBackend>,
    supervisor_factory: Box<dyn SupervisorFactory>,
    hooks: Vec<Arc<dyn InjectionHook>>,
    event_sink: Option<Arc<dyn PaneEventSink>>,
    panes: Mutex<HashMap<PaneId, Pane>>,
}

impl PaneHost {
    /// `pub(crate)`——2a 经 mock parts 构造（测试）；Windows 用 `new_windows`。
    pub(crate) fn new(config: PaneHostConfig) -> Self {
        Self {
            backend: config.backend,
            supervisor_factory: config.supervisor_factory,
            hooks: config.hooks,
            event_sink: config.event_sink,
            panes: Mutex::new(HashMap::new()),
        }
    }

    /// 公开 Windows 构造器（cutover 2b-3）：装配 WindowsPaneBackend（ConPTY + DSR 应答）
    /// + JobObjectSupervisorFactory（整树监管）+ conflux 提供的注入钩子链 + 事件出口。
    /// conflux 经此构造 PaneHost（不接触 conmux 内部的 backend/supervisor 类型）。
    #[cfg(windows)]
    pub fn new_windows(
        hooks: Vec<Arc<dyn InjectionHook>>,
        event_sink: Arc<dyn PaneEventSink>,
    ) -> Self {
        Self::new(PaneHostConfig {
            backend: Box::new(crate::pane_win::WindowsPaneBackend),
            supervisor_factory: Box::new(crate::job::JobObjectSupervisorFactory),
            hooks,
            event_sink: Some(event_sink),
        })
    }

    /// spawn 一个 pane：backend.open → session.spawn → 监管器 assign（fail-closed，MF-4）
    /// → 注册。assign 失败 ⇒ 不注册（不产生无监管 pane）。读线程/capture 接线在 2b。
    pub fn spawn(&self, req: SpawnRequest) -> Result<PaneId, ConmuxError> {
        {
            let panes = self.panes.lock().expect("panes 锁未中毒");
            if panes.contains_key(&req.pane_id) {
                return Err(ConmuxError::SpawnFailed {
                    message: format!("pane_id 已存在: {}", req.pane_id.0),
                });
            }
        }

        let mut session = self.backend.open(req.size)?;
        let pid = session.spawn(&req.command)?;

        // 每 pane 一个监管器；assign 失败 = fail-closed（MF-4 cl.2）：best-effort kill
        // 已 spawn 的进程（无监管进程不得逃逸为孤儿，红队 MF-A）→ 返回 Err、不注册。
        let supervisor = self.supervisor_factory.create();
        if let Err(e) = supervisor.assign(pid) {
            session.kill_best_effort();
            return Err(ConmuxError::SupervisorError {
                message: format!("assign 失败，已 fail-closed 拒绝 pane（已尝试终结进程）: {e}"),
            });
        }

        let scrollback = Arc::new(Mutex::new(LineIndexedBuffer::new(DEFAULT_BUFFER_CAPACITY)));

        // D-1a：session 进入 Arc<Mutex>——inject/poll_exit 在表锁外经此句柄操作。
        let session = Arc::new(Mutex::new(session));

        // 读线程（仅当有事件出口 = Windows 路径；mock 路径 event_sink=None 跳过，2a 测试不受扰）：
        // pump_reader_with_dsr 读 PTY → 应答 DSR → feed scrollback → 推 PaneOutput；
        // EOF 推 PaneExited（D-2a：经 Weak 取精确退出码）。
        #[cfg(windows)]
        if let Some(sink) = self.event_sink.clone() {
            let (writer, reader) = {
                let mut s = session.lock().expect("session 锁未中毒");
                (s.protocol_writer(), s.take_reader())
            };
            if let Some(writer) = writer {
                match reader {
                    Ok(reader) => {
                        let pane_id = req.pane_id.clone();
                        let sb = Arc::clone(&scrollback);
                        // Weak：kill/drop 后读线程不得延长 session 寿命——否则 master 不释放、
                        // reader 永不 EOF、线程泄漏。upgrade 失败（pane 已移除）⇒ exit_code=None。
                        let session_weak = Arc::downgrade(&session);
                        std::thread::spawn(move || {
                            let mut seq: u64 = 0;
                            crate::pane_win::pump_reader_with_dsr(reader, writer, |chunk| {
                                sb.lock().expect("scrollback 锁").append(chunk);
                                seq += 1;
                                sink.on_notify(MuxNotify::PaneOutput {
                                    pane_id: pane_id.clone(),
                                    seq,
                                    data: chunk.to_vec(),
                                });
                            });
                            // pump 返回 = reader EOF（进程退出 / master drop）。
                            // D-2a：自然退出时 pane 仍在表中、session 存活 → try_exit_code 取精确码。
                            //
                            // 代际守卫（2026-06-12）：weak 升级失败 = session 已被
                            // kill/respawn 移出表（本读线程属旧代际）→ **作废退出事件**。
                            // 否则 respawn 后旧线程的迟到 PaneExited 会污染同 id 新 pane
                            // 的退出态（实测：conflux 退出条在重启后反复复现的根因）。
                            // 自然退出不受影响——pane 仍注册、session 存活、精确码可取。
                            match session_weak.upgrade() {
                                Some(s) => {
                                    let exit_code =
                                        s.lock().ok().and_then(|mut g| g.try_exit_code());
                                    sink.on_notify(MuxNotify::PaneExited { pane_id, exit_code });
                                }
                                None => { /* 旧代际：跳过 emit */ }
                            }
                        });
                    }
                    Err(_e) => {
                        // take_reader 失败：不起读线程（无输出事件），pane 仍可 inject/kill。
                        // conmux 无日志依赖；失败可观察性由 conflux sink 侧补（后续）。
                    }
                }
            }
        }

        let working_dir = req.command.cwd.clone().unwrap_or_default();
        let pane = Pane {
            session,
            inject_lock: Arc::new(Mutex::new(())),
            supervisor,
            lifecycle: PaneLifecycle::Running,
            pid: Some(pid),
            exit_code: None,
            adapter_id: req.adapter_id,
            display_name: req.display_name,
            working_dir,
            size: req.size,
            created_at: req.created_at,
            scrollback,
        };
        self.panes
            .lock()
            .expect("panes 锁未中毒")
            .insert(req.pane_id.clone(), pane);
        Ok(req.pane_id)
    }

    /// **唯一对外写入口**（MF-1）。顺序不变量（MF-6）：
    /// before_inject（全部钩子，任一 Err ⇒ 不写）→ session.write_all → after_inject。
    /// `source` 由调用方按**信道身份**传入（in-proc 命令边界硬编码 / V2 管道客户端身份），
    /// **不来自** `MuxOp::Send`（它无 source 字段，MF-2）。
    ///
    /// **D-1a 锁纪律（库级不变量）**：钩子**绝不在全局 panes 表锁内调用**——钩子可执行任意
    /// 消费方逻辑（审计落库 / policy 查询 / 回调 PaneHost 自身），表锁内调用会制造跨锁死锁。
    /// 表锁仅用于取句柄；钩子链 + 写在 per-pane `inject_lock` 串行段内执行（保证 MF-6 整序
    /// 原子 + 审计落库顺序 == 字节抵达顺序）。
    ///
    /// 语义注：pane 在「取句柄之后、写之前」被并发 kill 时，本次注入不再报 PaneNotFound，
    /// 而是 write_all 对已关闭 PTY 返回 Err（after_inject 收到 Failed）——句柄经 Arc 短暂
    /// 存活，不阻塞 kill。
    pub fn inject_stdin(
        &self,
        pane_id: &PaneId,
        data: &[u8],
        source: InjectionSource,
    ) -> Result<(), ConmuxError> {
        // 表锁内只取句柄，立即释放（D-1a）。
        let (inject_lock, session) = {
            let panes = self.panes.lock().expect("panes 锁未中毒");
            let pane = panes
                .get(pane_id)
                .ok_or_else(|| ConmuxError::PaneNotFound {
                    pane_id: pane_id.0.clone(),
                })?;
            (Arc::clone(&pane.inject_lock), Arc::clone(&pane.session))
        };
        let _serial = inject_lock.lock().expect("inject 锁未中毒");

        let ctx = InjectionContext {
            pane_id,
            source,
            byte_len: data.len(),
            content: data,
        };

        // MF-6 fail-closed：任一 before_inject Err ⇒ 字节绝不抵达 PTY。
        for hook in &self.hooks {
            if let Err(e) = hook.before_inject(&ctx) {
                // 通知 after_inject 该次被拒（结果即该 Err），便于审计追加 Failed。
                let rejected: Result<(), ConmuxError> = Err(e.clone());
                for h in &self.hooks {
                    h.after_inject(&ctx, &rejected);
                }
                return Err(e);
            }
        }

        let result = session.lock().expect("session 锁未中毒").write_all(data);
        for hook in &self.hooks {
            hook.after_inject(&ctx, &result);
        }
        result
    }

    /// 整树终结（走 supervisor.kill_tree，MF-4）。**无论 kill_tree 成败，pane 一律从表移除**
    /// （MF-4 cl.4：失败仍清理，调用方据返回的 Err 决定是否标 zombie/上报）。
    pub fn kill(&self, pane_id: &PaneId) -> Result<(), ConmuxError> {
        let pane = {
            let mut panes = self.panes.lock().expect("panes 锁未中毒");
            panes
                .remove(pane_id)
                .ok_or_else(|| ConmuxError::PaneNotFound {
                    pane_id: pane_id.0.clone(),
                })?
        };
        // pane 已移除（内部表干净）；kill_tree 结果回传调用方。session 随 pane drop 释放。
        pane.supervisor.kill_tree()
    }

    /// 在同一 pane_id 下重起（先 kill_tree 旧的——若存在——再 spawn）。
    pub fn respawn(&self, pane_id: &PaneId, req: SpawnRequest) -> Result<(), ConmuxError> {
        // 旧 pane 存在则整树终结（忽略 kill 错误：可能已自退）。
        if let Some(old) = self
            .panes
            .lock()
            .expect("panes 锁未中毒")
            .remove(pane_id)
        {
            let _ = old.supervisor.kill_tree();
        }
        // req.pane_id 应与 pane_id 一致（调用方保证）。
        self.spawn(req).map(|_| ())
    }

    pub fn resize(&self, pane_id: &PaneId, size: PaneSize) -> Result<(), ConmuxError> {
        let mut panes = self.panes.lock().expect("panes 锁未中毒");
        let pane = panes
            .get_mut(pane_id)
            .ok_or_else(|| ConmuxError::PaneNotFound {
                pane_id: pane_id.0.clone(),
            })?;
        pane.session.lock().expect("session 锁未中毒").resize(size)?;
        pane.size = size;
        Ok(())
    }

    /// 非阻塞退出检测（cutover ③ D-2a）。查到退出 ⇒ lifecycle 翻成 `Exited(code)` 并记
    /// `exit_code`（兑现契约 §3.3——此前 lifecycle 永远 Running 的语义缺口）；仍在运行 ⇒
    /// `Ok(None)`。
    ///
    /// 与 `PaneExited` 事件互补：ConPTY reader 在 child 退出后**可能不返回 EOF**（实测
    /// 已知），事件可能永不到达——消费方（conflux `is_process_exited` 轮询）必须有此兜底。
    pub fn poll_exit(&self, pane_id: &PaneId) -> Result<Option<i32>, ConmuxError> {
        let mut panes = self.panes.lock().expect("panes 锁未中毒");
        let pane = panes
            .get_mut(pane_id)
            .ok_or_else(|| ConmuxError::PaneNotFound {
                pane_id: pane_id.0.clone(),
            })?;
        if let PaneLifecycle::Exited(code) = pane.lifecycle {
            return Ok(Some(code));
        }
        let code = pane
            .session
            .lock()
            .expect("session 锁未中毒")
            .try_exit_code();
        if let Some(c) = code {
            pane.lifecycle = PaneLifecycle::Exited(c);
            pane.exit_code = Some(c);
        }
        Ok(code)
    }

    /// 对账/死亡检测用。
    pub fn list_panes(&self) -> Vec<PaneState> {
        let panes = self.panes.lock().expect("panes 锁未中毒");
        panes.iter().map(|(id, pane)| pane.to_state(id)).collect()
    }

    /// 单 pane 状态查询（V1-core：行级 jump-back 的 scrollback 高水位取数路径——
    /// ingest 每事件一查，避免 list_panes O(n)）。
    pub fn pane_state(&self, pane_id: &PaneId) -> Result<PaneState, ConmuxError> {
        let panes = self.panes.lock().expect("panes 锁未中毒");
        panes
            .get(pane_id)
            .map(|pane| pane.to_state(pane_id))
            .ok_or_else(|| ConmuxError::PaneNotFound {
                pane_id: pane_id.0.clone(),
            })
    }

    /// 捕获 pane scrollback（契约 §3.4 / §6）。ANSI 开关：`ansi=false` 剥离 VT 序列
    /// （喂 LLM / 搜索）；`true` 保留原始。替代现状 manager.get_buffer 的历史读取。
    ///
    /// **读审计（C2）**：`CaptureResult.effectively_full` 由 `is_effectively_full` 算出
    /// （机制层判定），conflux 据此写 `CaptureDump` read 审计（审计存储属 conflux 策略）。
    pub fn capture(
        &self,
        req: crate::capture::CaptureRequest,
    ) -> Result<crate::capture::CaptureResult, ConmuxError> {
        use crate::capture::CaptureRange;
        let panes = self.panes.lock().expect("panes 锁未中毒");
        let pane = panes
            .get(&req.pane_id)
            .ok_or_else(|| ConmuxError::PaneNotFound {
                pane_id: req.pane_id.0.clone(),
            })?;
        let sb = pane.scrollback.lock().expect("scrollback 锁未中毒");
        let (first, last) = sb.line_range_available();

        // 取字节 + truncated 判定（LineRange 被环覆盖 → None → truncated）。
        let (raw, truncated) = match &req.range {
            CaptureRange::All => (sb.read_all_bytes(), false),
            CaptureRange::LastBytes(n) => {
                let valid = sb.total_bytes() as usize;
                (sb.read_last_bytes(*n), *n > valid)
            }
            CaptureRange::LineRange { start_abs, end_abs } => {
                // read_lines 是 [start, end)；契约 end_abs 含端 → +1。
                match sb.read_lines(*start_abs, end_abs.saturating_add(1)) {
                    Some(bytes) => (bytes, false),
                    None => (Vec::new(), true), // 起始已被环覆盖，不静默返部分
                }
            }
        };

        let data = if req.ansi {
            raw
        } else {
            crate::capture::strip_ansi(&raw)
        };
        let data_base64 = base64_encode(&data);

        // 等效全量判定（复闸 C2）：按有效覆盖而非枚举变体，杜绝换 range 规避审计。
        let effectively_full = crate::capture::is_effectively_full(
            &req.range,
            sb.total_bytes() as usize,
            first,
            last,
        );

        Ok(crate::capture::CaptureResult {
            data_base64,
            first_abs_line: first,
            last_abs_line: last,
            truncated,
            effectively_full,
        })
    }
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    // ===== Mock backend / session =====

    #[derive(Default)]
    struct MockSessionState {
        written: Vec<Vec<u8>>,
        reader_taken: bool,
        resized_to: Option<PaneSize>,
        spawn_should_fail: bool,
        /// D-2a：模拟 try_exit_code 返回值（None = 仍在运行）。
        exit_code: Option<i32>,
        /// 红队 MF-A：记录 kill_best_effort 被调次数（assign 失败应触发）。
        kill_best_effort_calls: u32,
    }

    #[derive(Clone)]
    struct MockSession {
        state: Arc<StdMutex<MockSessionState>>,
        pid: u32,
    }

    impl PaneSession for MockSession {
        fn spawn(&mut self, _cmd: &CommandSpec) -> Result<u32, ConmuxError> {
            if self.state.lock().unwrap().spawn_should_fail {
                return Err(ConmuxError::SpawnFailed {
                    message: "mock spawn fail".into(),
                });
            }
            Ok(self.pid)
        }
        fn take_reader(&mut self) -> Result<Box<dyn std::io::Read + Send>, ConmuxError> {
            let mut s = self.state.lock().unwrap();
            if s.reader_taken {
                return Err(ConmuxError::PtyError {
                    message: "reader 已被移交".into(),
                });
            }
            s.reader_taken = true;
            Ok(Box::new(std::io::empty()))
        }
        fn resize(&self, size: PaneSize) -> Result<(), ConmuxError> {
            self.state.lock().unwrap().resized_to = Some(size);
            Ok(())
        }
        fn write_all(&mut self, data: &[u8]) -> Result<(), ConmuxError> {
            self.state.lock().unwrap().written.push(data.to_vec());
            Ok(())
        }
        fn try_exit_code(&mut self) -> Option<i32> {
            self.state.lock().unwrap().exit_code
        }
        fn kill_best_effort(&mut self) {
            self.state.lock().unwrap().kill_best_effort_calls += 1;
        }
    }

    struct MockBackend {
        state: Arc<StdMutex<MockSessionState>>,
        pid: u32,
    }
    impl PaneBackend for MockBackend {
        fn open(&self, _size: PaneSize) -> Result<Box<dyn PaneSession>, ConmuxError> {
            Ok(Box::new(MockSession {
                state: Arc::clone(&self.state),
                pid: self.pid,
            }))
        }
    }

    // ===== Mock supervisor (records assign/kill_tree via shared state) =====

    #[derive(Default)]
    struct SupervisorRecord {
        assigned_pids: Vec<u32>,
        kill_tree_calls: u32,
        assign_should_fail: bool,
        kill_tree_should_fail: bool,
    }

    struct MockSupervisor {
        rec: Arc<StdMutex<SupervisorRecord>>,
    }
    impl ProcessSupervisor for MockSupervisor {
        fn assign(&self, pid: u32) -> Result<(), ConmuxError> {
            let mut r = self.rec.lock().unwrap();
            if r.assign_should_fail {
                return Err(ConmuxError::SupervisorError {
                    message: "mock assign fail".into(),
                });
            }
            r.assigned_pids.push(pid);
            Ok(())
        }
        fn kill_tree(&self) -> Result<(), ConmuxError> {
            let mut r = self.rec.lock().unwrap();
            r.kill_tree_calls += 1;
            if r.kill_tree_should_fail {
                return Err(ConmuxError::SupervisorError {
                    message: "mock kill_tree fail".into(),
                });
            }
            Ok(())
        }
    }

    struct MockSupervisorFactory {
        rec: Arc<StdMutex<SupervisorRecord>>,
    }
    impl SupervisorFactory for MockSupervisorFactory {
        fn create(&self) -> Box<dyn ProcessSupervisor> {
            Box::new(MockSupervisor {
                rec: Arc::clone(&self.rec),
            })
        }
    }

    // ===== Recording injection hook =====

    #[derive(Default)]
    struct HookRecord {
        before_calls: Vec<(String, InjectionSource, usize)>, // (pane_id, source, byte_len)
        after_calls: Vec<bool>,                              // result.is_ok()
        before_should_fail: bool,
    }
    struct RecordingHook {
        rec: Arc<StdMutex<HookRecord>>,
        /// 与 session 共享，用于断言"before_inject Err 时 write_all 未被调用"。
        session_state: Arc<StdMutex<MockSessionState>>,
    }
    impl InjectionHook for RecordingHook {
        fn before_inject(&self, ctx: &InjectionContext) -> Result<(), ConmuxError> {
            let mut r = self.rec.lock().unwrap();
            r.before_calls
                .push((ctx.pane_id.0.clone(), ctx.source.clone(), ctx.byte_len));
            if r.before_should_fail {
                // 断言点：此刻 write_all 必须还没发生。
                assert!(
                    self.session_state.lock().unwrap().written.is_empty(),
                    "fail-closed 破坏：before_inject 拒绝前不应已写 PTY"
                );
                return Err(ConmuxError::InjectionRejected {
                    reason: "mock reject".into(),
                });
            }
            Ok(())
        }
        fn after_inject(&self, _ctx: &InjectionContext, result: &Result<(), ConmuxError>) {
            self.rec.lock().unwrap().after_calls.push(result.is_ok());
        }
    }

    // ===== 测试夹具 =====

    struct Fixture {
        host: PaneHost,
        session_state: Arc<StdMutex<MockSessionState>>,
        sup_rec: Arc<StdMutex<SupervisorRecord>>,
        hook_rec: Arc<StdMutex<HookRecord>>,
    }

    fn fixture_with_pid(pid: u32) -> Fixture {
        let session_state = Arc::new(StdMutex::new(MockSessionState::default()));
        let sup_rec = Arc::new(StdMutex::new(SupervisorRecord::default()));
        let hook_rec = Arc::new(StdMutex::new(HookRecord::default()));
        let hook = RecordingHook {
            rec: Arc::clone(&hook_rec),
            session_state: Arc::clone(&session_state),
        };
        let host = PaneHost::new(PaneHostConfig {
            backend: Box::new(MockBackend {
                state: Arc::clone(&session_state),
                pid,
            }),
            supervisor_factory: Box::new(MockSupervisorFactory {
                rec: Arc::clone(&sup_rec),
            }),
            hooks: vec![Arc::new(hook)],
            event_sink: None, // mock 路径不起读线程
        });
        Fixture {
            host,
            session_state,
            sup_rec,
            hook_rec,
        }
    }

    fn req(id: &str) -> SpawnRequest {
        SpawnRequest {
            pane_id: PaneId(id.into()),
            command: CommandSpec {
                program: "cmd.exe".into(),
                args: vec![],
                cwd: Some("D:\\repo".into()),
                env: vec![],
            },
            size: PaneSize { rows: 24, cols: 80 },
            adapter_id: "claude-code".into(),
            display_name: Some("rev".into()),
            created_at: 1_700_000_000,
        }
    }

    #[test]
    fn spawn_registers_running_pane_and_assigns_supervisor() {
        let f = fixture_with_pid(4242);
        let id = f.host.spawn(req("p1")).unwrap();
        assert_eq!(id, PaneId("p1".into()));
        let panes = f.host.list_panes();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].lifecycle, PaneLifecycle::Running);
        assert_eq!(panes[0].pid, Some(4242));
        assert_eq!(panes[0].adapter_id, "claude-code");
        assert_eq!(panes[0].working_dir, "D:\\repo");
        // 监管器被创建并 assign 了该 pid（MF-4）。
        assert_eq!(f.sup_rec.lock().unwrap().assigned_pids, vec![4242]);
    }

    #[test]
    fn spawn_duplicate_id_rejected() {
        let f = fixture_with_pid(1);
        f.host.spawn(req("dup")).unwrap();
        assert!(matches!(
            f.host.spawn(req("dup")),
            Err(ConmuxError::SpawnFailed { .. })
        ));
    }

    #[test]
    fn spawn_assign_failure_is_fail_closed_no_pane_registered() {
        let f = fixture_with_pid(7);
        f.sup_rec.lock().unwrap().assign_should_fail = true;
        let r = f.host.spawn(req("p"));
        assert!(matches!(r, Err(ConmuxError::SupervisorError { .. })));
        assert!(
            f.host.list_panes().is_empty(),
            "assign 失败不得产生无监管 pane（MF-4 cl.2）"
        );
        // 红队 MF-A：已 spawn 的进程必须被 best-effort kill（不留无监管孤儿）。
        assert_eq!(
            f.session_state.lock().unwrap().kill_best_effort_calls,
            1,
            "assign 失败必须 best-effort kill 已 spawn 进程"
        );
    }

    #[test]
    fn spawn_backend_failure_propagates() {
        let f = fixture_with_pid(1);
        f.session_state.lock().unwrap().spawn_should_fail = true;
        assert!(matches!(
            f.host.spawn(req("p")),
            Err(ConmuxError::SpawnFailed { .. })
        ));
        assert!(f.host.list_panes().is_empty());
    }

    #[test]
    fn inject_routes_through_hook_then_write_in_order() {
        let f = fixture_with_pid(1);
        f.host.spawn(req("p1")).unwrap();
        f.host
            .inject_stdin(&PaneId("p1".into()), b"hello", InjectionSource::UserDirect)
            .unwrap();
        // before_inject 被调用，带正确 pane_id/source/byte_len（MF-2/MF-3 上下文）。
        let hr = f.hook_rec.lock().unwrap();
        assert_eq!(hr.before_calls.len(), 1);
        assert_eq!(hr.before_calls[0].0, "p1");
        assert_eq!(hr.before_calls[0].1, InjectionSource::UserDirect);
        assert_eq!(hr.before_calls[0].2, 5);
        assert_eq!(hr.after_calls, vec![true]);
        // write_all 收到字节（唯一写链 hook → inject_stdin → write_all）。
        assert_eq!(f.session_state.lock().unwrap().written, vec![b"hello".to_vec()]);
    }

    #[test]
    fn inject_fail_closed_when_before_hook_rejects() {
        let f = fixture_with_pid(1);
        f.host.spawn(req("p1")).unwrap();
        f.hook_rec.lock().unwrap().before_should_fail = true;
        let r = f
            .host
            .inject_stdin(&PaneId("p1".into()), b"x", InjectionSource::OrchestrationAuto);
        assert!(matches!(r, Err(ConmuxError::InjectionRejected { .. })));
        // 关键：被拒后字节绝不抵达 PTY（MF-6 fail-closed）。
        assert!(
            f.session_state.lock().unwrap().written.is_empty(),
            "before_inject 拒绝 ⇒ write_all 必须未被调用"
        );
        // after_inject 仍被通知（结果为 Err），便于审计追加 Failed。
        assert_eq!(f.hook_rec.lock().unwrap().after_calls, vec![false]);
    }

    #[test]
    fn inject_source_is_caller_channel_identity_not_overridable() {
        // source 由调用方（信道身份）传入并原样进 ctx；不同信道身份得不同 source。
        let f = fixture_with_pid(1);
        f.host.spawn(req("p1")).unwrap();
        for src in [
            InjectionSource::UserDirect,
            InjectionSource::PermissionResponse,
            InjectionSource::DiscussionUserMessage,
        ] {
            f.host
                .inject_stdin(&PaneId("p1".into()), b"a", src.clone())
                .unwrap();
        }
        let seen: Vec<_> = f
            .hook_rec
            .lock()
            .unwrap()
            .before_calls
            .iter()
            .map(|(_, s, _)| s.clone())
            .collect();
        assert_eq!(
            seen,
            vec![
                InjectionSource::UserDirect,
                InjectionSource::PermissionResponse,
                InjectionSource::DiscussionUserMessage,
            ]
        );
    }

    #[test]
    fn inject_unknown_pane_returns_not_found() {
        let f = fixture_with_pid(1);
        assert!(matches!(
            f.host
                .inject_stdin(&PaneId("nope".into()), b"x", InjectionSource::UserDirect),
            Err(ConmuxError::PaneNotFound { .. })
        ));
    }

    #[test]
    fn kill_calls_kill_tree_and_removes_pane() {
        let f = fixture_with_pid(1);
        f.host.spawn(req("p1")).unwrap();
        f.host.kill(&PaneId("p1".into())).unwrap();
        assert_eq!(f.sup_rec.lock().unwrap().kill_tree_calls, 1);
        assert!(f.host.list_panes().is_empty());
    }

    #[test]
    fn kill_tree_failure_still_removes_pane_and_returns_err() {
        let f = fixture_with_pid(1);
        f.host.spawn(req("p1")).unwrap();
        f.sup_rec.lock().unwrap().kill_tree_should_fail = true;
        let r = f.host.kill(&PaneId("p1".into()));
        assert!(matches!(r, Err(ConmuxError::SupervisorError { .. })));
        // MF-4 cl.4：kill_tree 失败仍清理内部表（无 ghost）。
        assert!(f.host.list_panes().is_empty());
    }

    #[test]
    fn kill_unknown_pane_returns_not_found() {
        let f = fixture_with_pid(1);
        assert!(matches!(
            f.host.kill(&PaneId("nope".into())),
            Err(ConmuxError::PaneNotFound { .. })
        ));
    }

    #[test]
    fn resize_updates_size_and_calls_session() {
        let f = fixture_with_pid(1);
        f.host.spawn(req("p1")).unwrap();
        let ns = PaneSize { rows: 40, cols: 120 };
        f.host.resize(&PaneId("p1".into()), ns).unwrap();
        assert_eq!(f.session_state.lock().unwrap().resized_to, Some(ns));
        assert_eq!(f.host.list_panes()[0].size, ns);
        assert!(matches!(
            f.host.resize(&PaneId("nope".into()), ns),
            Err(ConmuxError::PaneNotFound { .. })
        ));
    }

    #[test]
    fn respawn_reuses_id_and_kills_old() {
        let f = fixture_with_pid(99);
        f.host.spawn(req("p1")).unwrap();
        f.host.respawn(&PaneId("p1".into()), req("p1")).unwrap();
        // 旧 pane 被 kill_tree（respawn 内），新 pane 复用同 id。
        assert!(f.sup_rec.lock().unwrap().kill_tree_calls >= 1);
        let panes = f.host.list_panes();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].pane_id, PaneId("p1".into()));
    }

    #[test]
    fn pane_session_take_reader_is_one_shot() {
        // MF-1：读端一次性移交，二次返回 Err（读句柄不可重复外发）。
        let state = Arc::new(StdMutex::new(MockSessionState::default()));
        let mut session = MockSession {
            state,
            pid: 1,
        };
        assert!(session.take_reader().is_ok());
        assert!(session.take_reader().is_err(), "二次 take_reader 必须 Err");
    }

    #[test]
    fn panehost_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PaneHost>();
    }

    // ===== D-1a：钩子不在全局表锁内调用（cutover ③ 锁纪律回归） =====

    /// 钩子在 before_inject 里回调 PaneHost 自身（list_panes 取表锁）。
    /// 旧实现持表锁全程调钩子 ⇒ 此处自死锁；D-1a 后必须正常完成。
    struct ReentrantHook {
        host: Arc<StdMutex<Option<Arc<PaneHost>>>>,
        reentered: Arc<StdMutex<bool>>,
    }
    impl InjectionHook for ReentrantHook {
        fn before_inject(&self, _ctx: &InjectionContext) -> Result<(), ConmuxError> {
            if let Some(host) = self.host.lock().unwrap().as_ref() {
                let _ = host.list_panes(); // 表锁内调钩子时此行死锁
                *self.reentered.lock().unwrap() = true;
            }
            Ok(())
        }
    }

    #[test]
    fn inject_hook_may_reenter_panehost_without_deadlock() {
        let session_state = Arc::new(StdMutex::new(MockSessionState::default()));
        let sup_rec = Arc::new(StdMutex::new(SupervisorRecord::default()));
        let host_slot: Arc<StdMutex<Option<Arc<PaneHost>>>> = Arc::new(StdMutex::new(None));
        let reentered = Arc::new(StdMutex::new(false));
        let host = Arc::new(PaneHost::new(PaneHostConfig {
            backend: Box::new(MockBackend {
                state: Arc::clone(&session_state),
                pid: 1,
            }),
            supervisor_factory: Box::new(MockSupervisorFactory {
                rec: Arc::clone(&sup_rec),
            }),
            hooks: vec![Arc::new(ReentrantHook {
                host: Arc::clone(&host_slot),
                reentered: Arc::clone(&reentered),
            })],
            event_sink: None,
        }));
        *host_slot.lock().unwrap() = Some(Arc::clone(&host));
        host.spawn(req("p1")).unwrap();

        // 在子线程跑 inject + 超时看护：死锁时 recv 超时而非测试挂死。
        let (tx, rx) = std::sync::mpsc::channel();
        let h2 = Arc::clone(&host);
        std::thread::spawn(move || {
            let r = h2.inject_stdin(&PaneId("p1".into()), b"x", InjectionSource::UserDirect);
            let _ = tx.send(r);
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("inject 应在 5s 内完成（超时 = 钩子在表锁内被调用 → 死锁回归）");
        assert!(result.is_ok());
        assert!(*reentered.lock().unwrap(), "钩子应成功回调 list_panes");
        // 字节仍正常抵达（锁外钩子不破坏唯一写链）。
        assert_eq!(session_state.lock().unwrap().written, vec![b"x".to_vec()]);
    }

    // ===== D-2a：poll_exit 退出检测兜底 =====

    #[test]
    fn poll_exit_running_returns_none_and_keeps_lifecycle() {
        let f = fixture_with_pid(1);
        f.host.spawn(req("p1")).unwrap();
        assert_eq!(f.host.poll_exit(&PaneId("p1".into())).unwrap(), None);
        assert_eq!(f.host.list_panes()[0].lifecycle, PaneLifecycle::Running);
    }

    #[test]
    fn poll_exit_flips_lifecycle_and_records_exit_code() {
        let f = fixture_with_pid(1);
        f.host.spawn(req("p1")).unwrap();
        f.session_state.lock().unwrap().exit_code = Some(7);
        assert_eq!(f.host.poll_exit(&PaneId("p1".into())).unwrap(), Some(7));
        let st = &f.host.list_panes()[0];
        assert_eq!(st.lifecycle, PaneLifecycle::Exited(7));
        assert_eq!(st.exit_code, Some(7));
        // 已 Exited 后走缓存路径（即使 session 不再报码也稳定返回）。
        f.session_state.lock().unwrap().exit_code = None;
        assert_eq!(f.host.poll_exit(&PaneId("p1".into())).unwrap(), Some(7));
    }

    #[test]
    fn poll_exit_unknown_pane_returns_not_found() {
        let f = fixture_with_pid(1);
        assert!(matches!(
            f.host.poll_exit(&PaneId("nope".into())),
            Err(ConmuxError::PaneNotFound { .. })
        ));
    }

    /// V1-core：单 pane 状态查询（行级 jump-back 高水位取数路径）。
    #[test]
    fn pane_state_returns_single_pane_or_not_found() {
        let f = fixture_with_pid(7);
        f.host.spawn(req("p1")).unwrap();
        let st = f.host.pane_state(&PaneId("p1".into())).unwrap();
        assert_eq!(st.pane_id, PaneId("p1".into()));
        assert_eq!(st.pid, Some(7));
        assert!(matches!(
            f.host.pane_state(&PaneId("nope".into())),
            Err(ConmuxError::PaneNotFound { .. })
        ));
    }

    // ===== Windows 端到端集成（cutover 2b-3）：new_windows 真实组装 =====
    #[cfg(windows)]
    mod windows_e2e {
        use super::super::*;
        use crate::event::{MuxNotify, PaneEventSink};
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        struct CollectSink {
            events: Arc<Mutex<Vec<MuxNotify>>>,
        }
        impl PaneEventSink for CollectSink {
            fn on_notify(&self, notify: MuxNotify) {
                self.events.lock().unwrap().push(notify);
            }
        }

        fn win_req(id: &str, echo: &str) -> SpawnRequest {
            SpawnRequest {
                pane_id: PaneId(id.into()),
                command: CommandSpec {
                    program: "cmd.exe".into(),
                    args: vec!["/c".into(), format!("echo {echo}")],
                    cwd: None,
                    env: vec![],
                },
                size: PaneSize { rows: 24, cols: 80 },
                adapter_id: "shell".into(),
                display_name: None,
                created_at: 0,
            }
        }

        /// 完整 Windows 组装：new_windows → spawn（JobObject assign + ConPTY + DSR 读线程）
        /// → 收到 PaneOutput（含 echo marker，证明 DSR 应答 + 读线程 + 事件链通）→ kill。
        #[test]
        fn new_windows_spawn_emits_output_then_kill() {
            let events = Arc::new(Mutex::new(Vec::new()));
            let host = PaneHost::new_windows(
                vec![],
                Arc::new(CollectSink {
                    events: Arc::clone(&events),
                }),
            );
            host.spawn(win_req("w1", "conmux-2b3-e2e")).expect("spawn 应成功");

            // 给 echo 跑完 + DSR 应答 + PaneOutput 流动的时间。
            std::thread::sleep(Duration::from_millis(1800));

            let collected: Vec<u8> = events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    MuxNotify::PaneOutput { data, .. } => Some(data.clone()),
                    _ => None,
                })
                .flatten()
                .collect();
            let text = String::from_utf8_lossy(&collected);
            assert!(
                text.contains("conmux-2b3-e2e"),
                "应收到含 echo marker 的 PaneOutput（DSR 已应答否则挂死），实际:\n{text}"
            );

            // V1-1：seq 单调——per-pane 从 1 起严格 +1 递增（V2 重放对账前提）。
            let seqs: Vec<u64> = events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    MuxNotify::PaneOutput { seq, .. } => Some(*seq),
                    _ => None,
                })
                .collect();
            assert!(!seqs.is_empty());
            assert_eq!(seqs[0], 1, "seq 从 1 起");
            assert!(
                seqs.windows(2).all(|w| w[1] == w[0] + 1),
                "seq 必须严格 +1 递增（无缺口/乱序），实际: {seqs:?}"
            );

            // kill：移除 pane → drop session（master）→ 读线程 EOF → PaneExited；
            // supervisor.kill_tree 整树终结。
            host.kill(&PaneId("w1".into())).expect("kill 应成功");
            assert!(host.list_panes().is_empty());
        }

        /// 注入经唯一写链到达真实 ConPTY（cmd 回显注入内容）。
        #[test]
        fn new_windows_inject_reaches_pty() {
            let events = Arc::new(Mutex::new(Vec::new()));
            let host = PaneHost::new_windows(
                vec![],
                Arc::new(CollectSink {
                    events: Arc::clone(&events),
                }),
            );
            // 交互式 cmd（无 /c），可接收注入。
            host.spawn(SpawnRequest {
                pane_id: PaneId("w2".into()),
                command: CommandSpec {
                    program: "cmd.exe".into(),
                    args: vec![],
                    cwd: None,
                    env: vec![],
                },
                size: PaneSize { rows: 24, cols: 80 },
                adapter_id: "shell".into(),
                display_name: None,
                created_at: 0,
            })
            .unwrap();
            std::thread::sleep(Duration::from_millis(800));
            host.inject_stdin(
                &PaneId("w2".into()),
                b"echo conmux-inject-2b3\r\n",
                InjectionSource::UserDirect,
            )
            .expect("inject 应成功");
            std::thread::sleep(Duration::from_millis(1500));

            let collected: Vec<u8> = events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    MuxNotify::PaneOutput { data, .. } => Some(data.clone()),
                    _ => None,
                })
                .flatten()
                .collect();
            let text = String::from_utf8_lossy(&collected);
            assert!(
                text.contains("conmux-inject-2b3"),
                "注入内容应经唯一写链到达 ConPTY 并回显，实际:\n{text}"
            );
            host.kill(&PaneId("w2".into())).unwrap();
        }

        /// capture：spawn echo → 输出进 scrollback → capture(All, ansi=false) 读回含 marker。
        #[test]
        fn new_windows_capture_reads_scrollback() {
            use crate::capture::{CaptureRange, CaptureRequest};
            use base64::Engine;

            let events = Arc::new(Mutex::new(Vec::new()));
            let host = PaneHost::new_windows(
                vec![],
                Arc::new(CollectSink {
                    events: Arc::clone(&events),
                }),
            );
            host.spawn(win_req("w3", "conmux-capture-2b3b")).unwrap();
            std::thread::sleep(Duration::from_millis(1800));

            let result = host
                .capture(CaptureRequest {
                    pane_id: PaneId("w3".into()),
                    range: CaptureRange::All,
                    ansi: false, // 剥离 VT
                })
                .expect("capture 应成功");
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(result.data_base64.as_bytes())
                .expect("base64 应可解");
            let text = String::from_utf8_lossy(&decoded);
            assert!(
                text.contains("conmux-capture-2b3b"),
                "capture 应读回 scrollback 内容，实际:\n{text}"
            );
            assert!(!result.truncated, "All 范围不应 truncated");
            assert!(result.effectively_full, "All 范围等效全量（触发 read 审计）");

            // 未知 pane → PaneNotFound。
            assert!(matches!(
                host.capture(CaptureRequest {
                    pane_id: PaneId("nope".into()),
                    range: CaptureRange::All,
                    ansi: true,
                }),
                Err(ConmuxError::PaneNotFound { .. })
            ));

            host.kill(&PaneId("w3".into())).unwrap();
        }

        /// D-2a：自然退出后 PaneExited 事件携带精确退出码 + poll_exit 兜底返回同码。
        #[test]
        fn new_windows_exit_code_via_event_and_poll() {
            let events = Arc::new(Mutex::new(Vec::new()));
            let host = PaneHost::new_windows(
                vec![],
                Arc::new(CollectSink {
                    events: Arc::clone(&events),
                }),
            );
            host.spawn(SpawnRequest {
                pane_id: PaneId("w4".into()),
                command: CommandSpec {
                    program: "cmd.exe".into(),
                    args: vec!["/c".into(), "exit 5".into()],
                    cwd: None,
                    env: vec![],
                },
                size: PaneSize { rows: 24, cols: 80 },
                adapter_id: "shell".into(),
                display_name: None,
                created_at: 0,
            })
            .unwrap();
            std::thread::sleep(Duration::from_millis(2000));

            // poll_exit 兜底（不依赖 reader EOF 是否到达）。
            let polled = host.poll_exit(&PaneId("w4".into())).expect("poll_exit 应成功");
            assert_eq!(polled, Some(5), "cmd /c exit 5 的退出码应为 5");
            assert_eq!(host.list_panes()[0].lifecycle, PaneLifecycle::Exited(5));

            // PaneExited 事件若已到达（reader EOF），exit_code 必须同为 Some(5)，
            // 不得伪装 None（D9 诚实原则；EOF 未到达则跳过该断言——poll 已覆盖）。
            let exited: Vec<Option<i32>> = events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    MuxNotify::PaneExited { exit_code, .. } => Some(*exit_code),
                    _ => None,
                })
                .collect();
            if let Some(code) = exited.first() {
                assert_eq!(*code, Some(5), "PaneExited 应携带精确退出码");
            }

            host.kill(&PaneId("w4".into())).unwrap();
        }
    }
}
