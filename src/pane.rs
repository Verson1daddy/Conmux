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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::event::{MuxNotify, PaneEventSink};
use crate::inject::{InjectionContext, InjectionHook};
use crate::job::{ProcessSupervisor, SupervisorFactory};
use crate::scrollback::{LineIndexedBuffer, DEFAULT_BUFFER_CAPACITY};
use crate::types::{InjectionSource, PaneId, PaneLifecycle, PaneSize, PaneState, ScrollbackInfo};
use crate::ConmuxError;

/// 中毒容忍锁恢复（H-3 / M2a 红队 M2a-M1）。
///
/// 持锁线程 panic 会毒化 `Mutex`；裸 `.expect()` 会让之后**每个** `.lock()` 级联 panic，
/// daemon 沦为「全 pane 不可管理的活死状态」。本助手以 `into_inner()` 取回**被守护数据**
/// 续用，单点 panic 不传导成全域锁风暴（daemon 侧 `catch_unwind` 另保证连接线程 panic 不
/// 越界，二者叠加 = H-3 隔离）。
///
/// **为何「恢复续用」而非设计 D-7 的「受控自杀」**：`PaneHost` 是 conmux/conflux **共享库**
/// ——conflux 在 Tauri 进程内 in-proc 持有它，库层 `process::exit` 会杀掉整个 app。受控退出
/// 是 daemon（独立形态的策略层）的决策，不属机制库。PaneHost 锁内临界区皆短（HashMap
/// 增删查 / `Arc::clone` / 环形缓冲 memcpy），无破坏性中途态，恢复的数据一致可用。
fn recover<T>(e: PoisonError<MutexGuard<'_, T>>) -> MutexGuard<'_, T> {
    e.into_inner()
}

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

/// attach 原子快照（M2 设计 D-6）。`PaneHost::attach_snapshot` 在 scrollback 锁内原子取
/// `(history, last_seq)`，锁外组装；消费方据此重建画面：喂 `mode_preamble` → 喂 `history`
/// （原始 VT 字节）→ 按 `seq > last_seq` 连续喂 live `PaneOutput`。**承诺面**——嵌入 API +
/// wire `MuxPayload::AttachSnapshot` 的源数据（后者把字节 base64）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSnapshot {
    /// 非默认 VT 模式位合成前导（alt-screen/光标/鼠标/bracketed paste），见 [`PaneHost::mode_preamble`]。
    pub mode_preamble: Vec<u8>,
    /// ring 内全部有效**原始字节**（含 VT 序列，供 xterm 重放自愈）。
    pub history: Vec<u8>,
    /// 与 `history` **原子对应**的 PaneOutput 序号高水位（live 流去重锚，D-6）。
    pub last_seq: u64,
    /// 取快照时的 pane 状态。
    pub pane_state: PaneState,
}

/// `attach_snapshot` 在表锁内拷出的标量元字段（出表锁后组装 PaneState 用，避免表锁内读 scrollback）。
struct SnapshotMeta {
    adapter_id: String,
    display_name: Option<String>,
    lifecycle: PaneLifecycle,
    pid: Option<u32>,
    exit_code: Option<i32>,
    working_dir: String,
    size: PaneSize,
    created_at: i64,
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
    /// VT 私有模式跟踪（读线程 feed；attach/重放前导合成——M2 spike 裁决）。
    /// 锁纪律：与 scrollback 同类——纯内存短持有、锁内无 I/O 无回调（表锁例外前提）。
    modes: Arc<Mutex<crate::modes::ModeTracker>>,
}

impl Pane {
    /// 表锁内调：拷贝标量元字段 + 取 scrollback 句柄（**不读 scrollback 内容**，
    /// 不取 scrollback 锁）。配合 [`build_pane_state`] 在表锁外组装 `PaneState`，
    /// 消除"表锁内取 scrollback 锁"的长持锁风险（C-2 锁纪律）。
    fn snapshot_meta(&self) -> (Arc<Mutex<LineIndexedBuffer>>, PaneStateMeta) {
        (
            Arc::clone(&self.scrollback),
            PaneStateMeta {
                adapter_id: self.adapter_id.clone(),
                display_name: self.display_name.clone(),
                lifecycle: self.lifecycle.clone(),
                pid: self.pid,
                exit_code: self.exit_code,
                working_dir: self.working_dir.clone(),
                size: self.size,
                created_at: self.created_at,
            },
        )
    }
}

/// 表锁内拷贝的 pane 标量元字段（不含 scrollback——scrollback 句柄单独取，锁外读）。
struct PaneStateMeta {
    adapter_id: String,
    display_name: Option<String>,
    lifecycle: PaneLifecycle,
    pid: Option<u32>,
    exit_code: Option<i32>,
    working_dir: String,
    size: PaneSize,
    created_at: i64,
}

/// 表锁外调：读 scrollback 字段（**一次 lock 取齐** total_bytes + 行窗，消除旧 `to_state`
/// 的 2 次独立 lock）+ 组装 `PaneState`。
///
/// 一致性说明：`meta` 是表锁释放前的快照，`scrollback` 字段是锁外读——两者非原子，
/// scrollback 可能比 meta 稍新（读线程在表锁释放后追加）。这对现有消费方无影响：
/// `run_exit_sweep` 用 `pane_id` 列表驱动 poll_exit，不依赖跨 pane scrollback 原子性；
/// `list_panes` 的消费方（对账/展示）容忍单 pane 内的弱一致。
fn build_pane_state(
    pane_id: PaneId,
    scrollback: Arc<Mutex<LineIndexedBuffer>>,
    meta: PaneStateMeta,
) -> PaneState {
    let (total_bytes, first, last) = {
        let sb = scrollback.lock().unwrap_or_else(recover);
        let (first, last) = sb.line_range_available();
        (sb.total_bytes(), first, last)
    };
    PaneState {
        pane_id,
        adapter_id: meta.adapter_id,
        display_name: meta.display_name,
        lifecycle: meta.lifecycle,
        pid: meta.pid,
        exit_code: meta.exit_code,
        working_dir: meta.working_dir,
        size: meta.size,
        scrollback: ScrollbackInfo {
            total_bytes,
            first_abs_line: first,
            last_abs_line: last,
        },
        created_at: meta.created_at,
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
    /// 信任策略（Slice 2：None = 跳过信任校验，向后兼容；Some = spawn 入口校验）。
    pub(crate) trust: Option<Arc<dyn crate::trust::TrustPolicy>>,
}

/// 对外门面。私有持有 pane 表；唯一写入口 `inject_stdin`（MF-1）。
///
/// **C-2 锁纪律（库级不变量）**：`panes` 表锁是短临界区领导锁——持有期间**禁止**任何
/// 可能阻塞的调用：session 锁等待、write_all/resize/try_exit_code（ConPTY 节流 / 系统
/// 调用可长阻塞）、kill_tree、注入钩子。需要 session 的操作一律「句柄取出」（表锁内
/// clone Arc 后立即释放）再锁外执行；事后写回 pane 字段须重入表锁并以 `Arc::ptr_eq`
/// 验证代际（respawn 产生同 id 新 pane，旧代际写回一律作废）。违反此纪律 = 单 pane
/// 阻塞冻结全表、kill 逃生通道被堵（回归测试见 tests 模块 C-2 节）。
pub struct PaneHost {
    backend: Box<dyn PaneBackend>,
    supervisor_factory: Box<dyn SupervisorFactory>,
    hooks: Vec<Arc<dyn InjectionHook>>,
    event_sink: Option<Arc<dyn PaneEventSink>>,
    /// 信任策略（Slice 2：None = 跳过，向后兼容；Some = spawn 入口校验）。
    trust: Option<Arc<dyn crate::trust::TrustPolicy>>,
    panes: Mutex<HashMap<PaneId, Pane>>,
}

/// 识别 `cmd.exe /c <abs_path>` 包裹形态，返回真正要执行的 shim 绝对路径（Slice 3）。
///
/// conmux-app `create_session` 把 shim（.cmd/.bat/无后缀）包成
/// `program=cmd.exe + args=["/c", shim_abs, ...]`。本函数提取 args[1] 供 `PaneHost::spawn`
/// 追加验签——否则只验 cmd.exe（A 档永远过），shim 从不被验签。
///
/// 命中条件（全满足才返 Some）：
/// - `program` 文件名 == `cmd.exe`（大小写不敏感）——比较文件名而非全路径，避免硬编 System32。
/// - `args.len() >= 2` 且 `args[0]` == `/c`（大小写不敏感）。
/// - `args[1]` 是绝对路径（相对路径/命令串如 `dir` 不触发——无文件可验，维持现状）。
///
/// **不命中 = 不收窄**：其它形态（`cmd /k`、`powershell -c`、`cmd /c dir`）维持现状
/// （仅 program 验过即放行），不引入新拒绝。
fn cmd_wrap_target(cmd: &CommandSpec) -> Option<String> {
    let is_cmd_exe = std::path::Path::new(&cmd.program)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.eq_ignore_ascii_case("cmd.exe"))
        .unwrap_or(false);
    if !is_cmd_exe {
        return None;
    }
    let args = &cmd.args;
    if args.len() < 2 || !args[0].eq_ignore_ascii_case("/c") {
        return None;
    }
    if std::path::Path::new(&args[1]).is_absolute() {
        Some(args[1].clone())
    } else {
        None
    }
}

impl PaneHost {
    /// `pub(crate)`——2a 经 mock parts 构造（测试）；Windows 用 `new_windows`。
    pub(crate) fn new(config: PaneHostConfig) -> Self {
        Self {
            backend: config.backend,
            supervisor_factory: config.supervisor_factory,
            hooks: config.hooks,
            event_sink: config.event_sink,
            trust: config.trust,
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
            trust: None, // 向后兼容：不校验信任（两条上层按需用 new_windows_with_trust）。
        })
    }

    /// 公开 Windows 构造器 + 信任策略注入（Slice 2）。
    /// 信任策略（TrustStore）启动时加载一次，经此注入；spawn 热路径不做文件 I/O。
    /// 两条上层（conmux-app / conflux-app）应优先用此构造器。
    #[cfg(windows)]
    pub fn new_windows_with_trust(
        hooks: Vec<Arc<dyn InjectionHook>>,
        event_sink: Arc<dyn PaneEventSink>,
        trust: Arc<dyn crate::trust::TrustPolicy>,
    ) -> Self {
        Self::new(PaneHostConfig {
            backend: Box::new(crate::pane_win::WindowsPaneBackend),
            supervisor_factory: Box::new(crate::job::JobObjectSupervisorFactory),
            hooks,
            event_sink: Some(event_sink),
            trust: Some(trust),
        })
    }

    /// spawn 一个 pane：backend.open → session.spawn → 监管器 assign（fail-closed，MF-4）
    /// → 注册。assign 失败 ⇒ 不注册（不产生无监管 pane）。读线程/capture 接线在 2b。
    pub fn spawn(&self, req: SpawnRequest) -> Result<PaneId, ConmuxError> {
        // Slice 1 守卫：到达内核的 program 必为绝对路径（消除"裸名 PATH 解析歧义"——
        // 注意**不**消除 verify↔spawn 之间文件被替换的 TOCTOU 窗口，见 trust.rs 已知限制 #3）。
        // 两条上层（conmux-app / conflux-app）负责把裸名解析成绝对路径；裸名透传 = 上游漏
        // 解析，fail-closed 拒绝（不丢给 CreateProcess 再猜）。Windows `Path::is_absolute()`
        // 对 `C:\...` / `\\server\share` 返回 true，对 `cmd`/`powershell.exe` 返回 false。
        if !std::path::Path::new(&req.command.program).is_absolute() {
            return Err(ConmuxError::NonAbsoluteProgram {
                program: req.command.program.clone(),
            });
        }

        // Slice 2 信任校验：对已解析的绝对路径 program 做三档决策（A 签名 / B 哈希钉 / C 拒）。
        // trust=None → 跳过（向后兼容）；trust=Some → 校验。TrustPolicy 内部处理 mode
        // （enforce/warn/off），warn/off 始终返 Allow，enforce 返真实决策。
        //
        // Slice 3 cmd-wrap 加固：conmux-app 把 shim（.cmd/.bat/无后缀）包成
        // `program=cmd.exe + args=["/c", shim_abs, ...]`。若只验 program=cmd.exe（A 档永远过），
        // args 里的 shim 从不被验签 → B 档哈希钉形同虚设。故在 program 验过后，识别
        // `cmd.exe /c <abs_path>` 形态、追加验签 <abs_path>（真正执行的 shim）。
        // 防绕过：program 先验（挡 C:\evil\cmd.exe 冒名）。conmux-app 自动包裹的 shim 恒为
        // 绝对路径（create_session 先 resolve_on_path 再包）→ 必命中、必验签，主威胁已堵。
        // 已知残留：args[1] 相对路径不验——`cmd /c dir` 是内建无文件可验；但 `cmd /c x.cmd`
        // 相对名 cmd 仍会按 PATH/cwd 解析并跑、却不被验签。仅当用户**手写**相对 cmd-wrap 命令
        // 时触发（自残式，非 方案 A 主威胁；自动包裹路径不受影响）。收窄需规范化相对 args 再验。
        if let Some(trust) = &self.trust {
            let path = std::path::Path::new(&req.command.program);
            match trust.verify(path) {
                crate::trust::TrustDecision::Allow => {}
                crate::trust::TrustDecision::Reject { reason } => {
                    return Err(ConmuxError::UntrustedProgram {
                        program: req.command.program.clone(),
                        reason,
                    });
                }
            }
            // cmd-wrap 追加验签：program 已过（真 cmd.exe）+ args[0]="/c" + args[1] 绝对路径 → 验 shim。
            if let Some(shim_path) = cmd_wrap_target(&req.command) {
                match trust.verify(std::path::Path::new(&shim_path)) {
                    crate::trust::TrustDecision::Allow => {}
                    crate::trust::TrustDecision::Reject { reason } => {
                        return Err(ConmuxError::UntrustedProgram {
                            program: shim_path,
                            reason,
                        });
                    }
                }
            }
        }

        {
            let panes = self.panes.lock().unwrap_or_else(recover);
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
        let modes = Arc::new(Mutex::new(crate::modes::ModeTracker::new()));

        // D-1a：session 进入 Arc<Mutex>——inject/poll_exit 在表锁外经此句柄操作。
        let session = Arc::new(Mutex::new(session));

        // 读线程（仅当有事件出口 = Windows 路径；mock 路径 event_sink=None 跳过，2a 测试不受扰）：
        // pump_reader_with_dsr 读 PTY → 应答 DSR → feed scrollback → 推 PaneOutput；
        // EOF 推 PaneExited（D-2a：经 Weak 取精确退出码）。
        #[cfg(windows)]
        if let Some(sink) = self.event_sink.clone() {
            let (writer, reader) = {
                let mut s = session.lock().unwrap_or_else(recover);
                (s.protocol_writer(), s.take_reader())
            };
            if let Some(writer) = writer {
                match reader {
                    Ok(reader) => {
                        let pane_id = req.pane_id.clone();
                        let sb = Arc::clone(&scrollback);
                        let md = Arc::clone(&modes);
                        // Weak：kill/drop 后读线程不得延长 session 寿命——否则 master 不释放、
                        // reader 永不 EOF、线程泄漏。upgrade 失败（pane 已移除）⇒ exit_code=None。
                        let session_weak = Arc::downgrade(&session);
                        std::thread::spawn(move || {
                            crate::pane_win::pump_reader_with_dsr(reader, writer, |chunk| {
                                // D-6：seq 在 scrollback 锁内与字节追加**原子绑定**——保证
                                // emit 的 seq=S 对应的 ring 状态必含本块，attach 快照锁内同读
                                // (read_all_bytes, seq) 即原子。emit 在锁外（H-1：序列化/投递不入锁）。
                                let seq = sb.lock().unwrap_or_else(recover).append_and_seq(chunk);
                                md.lock().unwrap_or_else(recover).feed(chunk);
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
            modes,
        };
        {
            let mut panes = self.panes.lock().unwrap_or_else(recover);
            match panes.entry(req.pane_id.clone()) {
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(pane);
                }
                std::collections::hash_map::Entry::Occupied(_) => {
                    // 并发同 id spawn 双双越过前置查重（TOCTOU）：fail-closed 终结后到者，
                    // 不覆盖已注册 pane（覆盖会静默 drop 先到者的监管器 = 静默杀树）。
                    drop(panes);
                    let _ = pane.supervisor.kill_tree();
                    return Err(ConmuxError::SpawnFailed {
                        message: format!("pane_id 已存在: {}", req.pane_id.0),
                    });
                }
            }
        }
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
            let panes = self.panes.lock().unwrap_or_else(recover);
            let pane = panes
                .get(pane_id)
                .ok_or_else(|| ConmuxError::PaneNotFound {
                    pane_id: pane_id.0.clone(),
                })?;
            (Arc::clone(&pane.inject_lock), Arc::clone(&pane.session))
        };
        let _serial = inject_lock.lock().unwrap_or_else(recover);

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

        let result = session.lock().unwrap_or_else(recover).write_all(data);
        for hook in &self.hooks {
            hook.after_inject(&ctx, &result);
        }
        result
    }

    /// 整树终结（走 supervisor.kill_tree，MF-4）。**无论 kill_tree 成败，pane 一律从表移除**
    /// （MF-4 cl.4：失败仍清理，调用方据返回的 Err 决定是否标 zombie/上报）。
    pub fn kill(&self, pane_id: &PaneId) -> Result<(), ConmuxError> {
        let pane = {
            let mut panes = self.panes.lock().unwrap_or_else(recover);
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
        // C-2：先 let 绑定再 kill——把 lock() 直接嵌进 if-let 头会让表锁 guard 延寿到
        // 块尾（临时生命周期延展），kill_tree 慢路径下冻结全表。
        let old = self
            .panes
            .lock()
            .unwrap_or_else(recover)
            .remove(pane_id);
        // 旧 pane 存在则整树终结（忽略 kill 错误：可能已自退）。
        if let Some(old) = old {
            let _ = old.supervisor.kill_tree();
        }
        // req.pane_id 应与 pane_id 一致（调用方保证）。
        self.spawn(req).map(|_| ())
    }

    pub fn resize(&self, pane_id: &PaneId, size: PaneSize) -> Result<(), ConmuxError> {
        // C-2 锁纪律：表锁内只取句柄；session 等待/IO 在锁外（同 inject_stdin D-1a）。
        let session = {
            let panes = self.panes.lock().unwrap_or_else(recover);
            let pane = panes
                .get(pane_id)
                .ok_or_else(|| ConmuxError::PaneNotFound {
                    pane_id: pane_id.0.clone(),
                })?;
            Arc::clone(&pane.session)
        };
        session.lock().unwrap_or_else(recover).resize(size)?;
        // 写回 size：重入表锁并以 Arc::ptr_eq 验证代际——句柄取出期间 pane 可能被
        // respawn（同 id 新 pane），旧代际的写回一律作废。
        let mut panes = self.panes.lock().unwrap_or_else(recover);
        if let Some(pane) = panes.get_mut(pane_id) {
            if Arc::ptr_eq(&pane.session, &session) {
                pane.size = size;
            }
        }
        Ok(())
    }

    /// 非阻塞退出检测（cutover ③ D-2a）。查到退出 ⇒ lifecycle 翻成 `Exited(code)` 并记
    /// `exit_code`（兑现契约 §3.3——此前 lifecycle 永远 Running 的语义缺口）；仍在运行 ⇒
    /// `Ok(None)`。
    ///
    /// 与 `PaneExited` 事件互补：ConPTY reader 在 child 退出后**可能不返回 EOF**（实测
    /// 已知），事件可能永不到达——消费方（conflux `is_process_exited` 轮询）必须有此兜底。
    pub fn poll_exit(&self, pane_id: &PaneId) -> Result<Option<i32>, ConmuxError> {
        // C-2 锁纪律：表锁内只读 lifecycle + 取句柄；try_exit_code（可能阻塞于系统调用）
        // 在锁外执行。
        let session = {
            let panes = self.panes.lock().unwrap_or_else(recover);
            let pane = panes
                .get(pane_id)
                .ok_or_else(|| ConmuxError::PaneNotFound {
                    pane_id: pane_id.0.clone(),
                })?;
            if let PaneLifecycle::Exited(code) = pane.lifecycle {
                return Ok(Some(code));
            }
            Arc::clone(&pane.session)
        };
        // try_lock 而非 lock：poll 语义是"本轮探测"，session 忙（如 write_all 阻塞于
        // ConPTY 节流）时返回"不可判定"让下轮重试——否则顺序轮询多 pane 的消费方会被
        // 单个忙 pane 卡死整轮（契约 L-1/4.3.1 看门狗要求 poll_exit 自身限时完成）。
        let code = match session.try_lock() {
            Ok(mut s) => s.try_exit_code(),
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
            // 中毒容忍（M2a-M1）：取回守护数据续探，不级联 panic（与 `recover` 同策略）。
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner().try_exit_code(),
        };
        let Some(c) = code else {
            return Ok(None);
        };
        // 写回：代际验证（与读线程 Weak 守卫同源语义）——句柄取出期间 pane 被
        // respawn/kill 时，旧代际的迟到退出码不得污染同 id 新 pane。
        let mut panes = self.panes.lock().unwrap_or_else(recover);
        match panes.get_mut(pane_id) {
            Some(pane) if Arc::ptr_eq(&pane.session, &session) => {
                pane.lifecycle = PaneLifecycle::Exited(c);
                pane.exit_code = Some(c);
                Ok(Some(c))
            }
            // 新代际运行中：旧结果作废，按当前代际报"未退出"。
            Some(_) => Ok(None),
            // pane 已被 kill：与"稍后调用"同语义。
            None => Err(ConmuxError::PaneNotFound {
                pane_id: pane_id.0.clone(),
            }),
        }
    }

    /// attach/重放前导（M2 spike 裁决）：当前 pane 非默认 VT 模式位的合成序列
    /// （alt-screen/光标可见性/鼠标/bracketed paste/DECCKM）。重放协议 =
    /// **本前导 + capture 字节**——模态状态是 ring 任意起点重放下唯一不自愈的部分
    /// （文本/光标经 TUI 绝对定位重绘自愈，spike 实证）。空 = 全默认态。
    pub fn mode_preamble(&self, pane_id: &PaneId) -> Result<Vec<u8>, ConmuxError> {
        // C-2 锁纪律：表锁内只取句柄；modes 锁为纯内存短持有。
        let modes = {
            let panes = self.panes.lock().unwrap_or_else(recover);
            let pane = panes
                .get(pane_id)
                .ok_or_else(|| ConmuxError::PaneNotFound {
                    pane_id: pane_id.0.clone(),
                })?;
            Arc::clone(&pane.modes)
        };
        let preamble = modes.lock().unwrap_or_else(recover).preamble();
        Ok(preamble)
    }

    /// attach 原子快照（M2 设计 D-6）。**无缝拼接不变量**：`history` 字节与 `last_seq` 在
    /// scrollback 锁内**同时**读取（原子对应），锁内仅做 memcpy 克隆 + 读 seq（H-1：base64 /
    /// JSON / 帧写出一律由调用方在锁外做，避免 1MB 级 CPU 工作进锁饿死读泵）。
    ///
    /// 调用方（daemon Attach 处理）须**先注册订阅、后取快照**：注册到快照间到达的事件按
    /// `seq > last_seq` 过滤即去重，保证无丢帧无重帧（D-6 客户端拼接契约）。
    pub fn attach_snapshot(&self, pane_id: &PaneId) -> Result<PaneSnapshot, ConmuxError> {
        // C-2 锁纪律：表锁内只取句柄 + 拷贝标量元字段，立即释放（不在表锁内读 scrollback）。
        let (scrollback, modes, meta) = {
            let panes = self.panes.lock().unwrap_or_else(recover);
            let pane = panes
                .get(pane_id)
                .ok_or_else(|| ConmuxError::PaneNotFound {
                    pane_id: pane_id.0.clone(),
                })?;
            (
                Arc::clone(&pane.scrollback),
                Arc::clone(&pane.modes),
                SnapshotMeta {
                    adapter_id: pane.adapter_id.clone(),
                    display_name: pane.display_name.clone(),
                    lifecycle: pane.lifecycle.clone(),
                    pid: pane.pid,
                    exit_code: pane.exit_code,
                    working_dir: pane.working_dir.clone(),
                    size: pane.size,
                    created_at: pane.created_at,
                },
            )
        };
        // 原子读 scrollback：(history, last_seq, 行窗) 同锁内取（H-1：仅 memcpy + 读字段）。
        let (history, last_seq, sb_info) = {
            let g = scrollback.lock().unwrap_or_else(recover);
            let (first, last) = g.line_range_available();
            (
                g.read_all_bytes(),
                g.seq(),
                ScrollbackInfo {
                    total_bytes: g.total_bytes(),
                    first_abs_line: first,
                    last_abs_line: last,
                },
            )
        };
        // 模式前导（独立短内存锁，与 scrollback 不嵌套）。
        let mode_preamble = modes.lock().unwrap_or_else(recover).preamble();
        let pane_state = PaneState {
            pane_id: pane_id.clone(),
            adapter_id: meta.adapter_id,
            display_name: meta.display_name,
            lifecycle: meta.lifecycle,
            pid: meta.pid,
            exit_code: meta.exit_code,
            working_dir: meta.working_dir,
            size: meta.size,
            scrollback: sb_info,
            created_at: meta.created_at,
        };
        Ok(PaneSnapshot {
            mode_preamble,
            history,
            last_seq,
            pane_state,
        })
    }

    /// 对账/死亡检测用。
    pub fn list_panes(&self) -> Vec<PaneState> {
        // C-2 锁纪律：表锁内只拷贝标量字段 + Arc::clone(&scrollback)，立即释放；
        // scrollback 字段在锁外逐 pane 读（消除表锁内 N 次取 scrollback 锁的长持锁）。
        let snapshots: Vec<(PaneId, Arc<Mutex<LineIndexedBuffer>>, PaneStateMeta)> = {
            let panes = self.panes.lock().unwrap_or_else(recover);
            panes
                .iter()
                .map(|(id, pane)| {
                    let (sb, meta) = pane.snapshot_meta();
                    (id.clone(), sb, meta)
                })
                .collect()
        };
        snapshots
            .into_iter()
            .map(|(id, sb, meta)| build_pane_state(id, sb, meta))
            .collect()
    }

    /// 单 pane 状态查询（V1-core：行级 jump-back 的 scrollback 高水位取数路径——
    /// ingest 每事件一查，避免 list_panes O(n)）。
    pub fn pane_state(&self, pane_id: &PaneId) -> Result<PaneState, ConmuxError> {
        // C-2 锁纪律：同 list_panes——表锁内只取句柄 + 拷贝标量，锁外读 scrollback。
        let (pane_id, scrollback, meta) = {
            let panes = self.panes.lock().unwrap_or_else(recover);
            let pane = panes
                .get(pane_id)
                .ok_or_else(|| ConmuxError::PaneNotFound {
                    pane_id: pane_id.0.clone(),
                })?;
            let (sb, meta) = pane.snapshot_meta();
            (pane_id.clone(), sb, meta)
        };
        Ok(build_pane_state(pane_id, scrollback, meta))
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
        // C-2 锁纪律：表锁内只取 scrollback 句柄，立即释放（对齐 attach_snapshot）。
        //
        // 代际守卫说明：capture **不写回 pane 字段**，与 resize/poll_exit 不同（它们
        // 要写回 size/lifecycle，故需 `Arc::ptr_eq` 验证代际防旧代际污染新 pane）。
        // 这里 Arc 引用计数保证：取句柄后即使 pane 被 kill+respawn（同 id 新 pane），
        // 旧 `Arc<scrollback>` 仍存活，读到的是取句柄那一刻的 scrollback，不会读到
        // 新 pane 的数据，也不会 panic。代际一致性天然成立，无需 ptr_eq 写回守卫
        // （回归测试 `capture_after_respawn_reads_old_generation_scrollback`）。
        let scrollback = {
            let panes = self.panes.lock().unwrap_or_else(recover);
            let pane = panes
                .get(&req.pane_id)
                .ok_or_else(|| ConmuxError::PaneNotFound {
                    pane_id: req.pane_id.0.clone(),
                })?;
            Arc::clone(&pane.scrollback)
        };
        // 锁外读 scrollback + base64 编码（H-1：重活移出表锁，避免冻结全表）。
        let (data_base64, first, last, truncated, effectively_full) = {
            let sb = scrollback.lock().unwrap_or_else(recover);
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
            (data_base64, first, last, truncated, effectively_full)
        };

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
        /// C-2：write_all 进入后停在此 gate（模拟 ConPTY 节流下的长阻塞写）。
        write_gate: Option<Arc<Gate>>,
        /// C-2：try_exit_code 进入后停在此 gate（构造代际竞态窗口）。
        exit_gate: Option<Arc<Gate>>,
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
            // gate 等待在 state 锁外（否则 mock 自身制造无关死锁）。
            let gate = self.state.lock().unwrap().write_gate.clone();
            if let Some(g) = gate {
                g.enter_and_wait();
            }
            self.state.lock().unwrap().written.push(data.to_vec());
            Ok(())
        }
        fn try_exit_code(&mut self) -> Option<i32> {
            let gate = self.state.lock().unwrap().exit_gate.clone();
            if let Some(g) = gate {
                g.enter_and_wait();
            }
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
        /// C-2：kill_tree 进入后停在此 gate（模拟 TerminateJobObject 慢路径）。
        kill_gate: Option<Arc<Gate>>,
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
            let gate = self.rec.lock().unwrap().kill_gate.clone();
            if let Some(g) = gate {
                g.enter_and_wait();
            }
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
            trust: None,     // mock 测试不校验信任
        });
        Fixture {
            host,
            session_state,
            sup_rec,
            hook_rec,
        }
    }

    fn req(id: &str) -> SpawnRequest {
        // Slice 1：内核 spawn 守卫要求 program 为绝对路径。MockSession.spawn 不检查 program
        // （用 `_cmd`），故用编译期绝对路径（CARGO_MANIFEST_DIR）满足守卫即可，文件无需存在。
        let abs_program = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("test-fake-cmd.exe")
            .to_string_lossy()
            .into_owned();
        SpawnRequest {
            pane_id: PaneId(id.into()),
            command: CommandSpec {
                program: abs_program,
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
    fn spawn_rejects_non_absolute_program() {
        // Slice 1 守卫：裸名 program → fail-closed NonAbsoluteProgram（不丢给 CreateProcess 猜）。
        let f = fixture_with_pid(1);
        let mut r = req("p1");
        r.command.program = "cmd.exe".to_string(); // 裸名
        assert!(matches!(
            f.host.spawn(r),
            Err(ConmuxError::NonAbsoluteProgram { .. })
        ));
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

    // ===== Slice 3：cmd-wrap 验签加固 =====
    //
    // conmux-app 把 shim（.cmd/.bat/无后缀）包成 `cmd.exe /c <shim_abs>`。原验签只校验
    // program=cmd.exe（A 档永远过），shim 在 args 里从不被验签。以下测试确认 spawn 现在
    // 把 shim 路径（而非 cmd.exe）送去二次验签，且 fail-closed 拒绝未 pin 的 shim。

    /// 记录被 verify 的路径，可配置拒绝集（测试 cmd-wrap 把 shim 而非 cmd.exe 送去验签）。
    struct RecordingTrust {
        seen: Arc<StdMutex<Vec<String>>>,
        reject: Vec<String>,
    }
    impl crate::trust::TrustPolicy for RecordingTrust {
        fn verify(&self, program: &std::path::Path) -> crate::trust::TrustDecision {
            let p = program.to_string_lossy().to_string();
            self.seen.lock().unwrap().push(p.clone());
            if self.reject.iter().any(|r| r.eq_ignore_ascii_case(&p)) {
                crate::trust::TrustDecision::Reject {
                    reason: format!("mock reject: {p}"),
                }
            } else {
                crate::trust::TrustDecision::Allow
            }
        }
    }

    fn abs_path(name: &str) -> String {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    /// 构造 `cmd.exe /c <shim_abs>` 形态的 SpawnRequest（模拟 conmux-app create_session 包 shim）。
    fn cmd_wrap_req(id: &str, shim_abs: &str) -> SpawnRequest {
        SpawnRequest {
            pane_id: PaneId(id.into()),
            command: CommandSpec {
                program: abs_path("cmd.exe"),
                args: vec!["/c".into(), shim_abs.into()],
                cwd: Some("D:\\repo".into()),
                env: vec![],
            },
            size: PaneSize { rows: 24, cols: 80 },
            adapter_id: "claude-code".into(),
            display_name: Some("rev".into()),
            created_at: 1_700_000_000,
        }
    }

    fn host_with_trust(trust: Arc<dyn crate::trust::TrustPolicy>) -> PaneHost {
        PaneHost::new(PaneHostConfig {
            backend: Box::new(MockBackend {
                state: Arc::new(StdMutex::new(MockSessionState::default())),
                pid: 1,
            }),
            supervisor_factory: Box::new(MockSupervisorFactory {
                rec: Arc::new(StdMutex::new(SupervisorRecord::default())),
            }),
            hooks: vec![],
            event_sink: None,
            trust: Some(trust),
        })
    }

    #[test]
    fn cmd_wrap_target_extracts_shim_path() {
        // 命中：cmd.exe /c <abs> → Some(abs)
        let cmd = CommandSpec {
            program: abs_path("cmd.exe"),
            args: vec!["/c".into(), abs_path("shim.cmd")],
            cwd: None,
            env: vec![],
        };
        assert_eq!(cmd_wrap_target(&cmd), Some(abs_path("shim.cmd")));

        // 大小写不敏感（CMD.EXE /C）
        let cmd = CommandSpec {
            program: abs_path("CMD.EXE"),
            args: vec!["/C".into(), abs_path("shim.cmd")],
            cwd: None,
            env: vec![],
        };
        assert_eq!(cmd_wrap_target(&cmd), Some(abs_path("shim.cmd")));

        // 非 cmd.exe（powershell）→ None
        let cmd = CommandSpec {
            program: abs_path("powershell.exe"),
            args: vec!["/c".into(), abs_path("shim.cmd")],
            cwd: None,
            env: vec![],
        };
        assert_eq!(cmd_wrap_target(&cmd), None);

        // /k 而非 /c → None
        let cmd = CommandSpec {
            program: abs_path("cmd.exe"),
            args: vec!["/k".into(), abs_path("shim.cmd")],
            cwd: None,
            env: vec![],
        };
        assert_eq!(cmd_wrap_target(&cmd), None);

        // 相对路径 args[1]（如 `cmd /c dir`）→ None（无文件可验，维持现状）
        let cmd = CommandSpec {
            program: abs_path("cmd.exe"),
            args: vec!["/c".into(), "dir".into()],
            cwd: None,
            env: vec![],
        };
        assert_eq!(cmd_wrap_target(&cmd), None);

        // args 不足 → None
        let cmd = CommandSpec {
            program: abs_path("cmd.exe"),
            args: vec!["/c".into()],
            cwd: None,
            env: vec![],
        };
        assert_eq!(cmd_wrap_target(&cmd), None);
    }

    #[test]
    fn spawn_cmd_wrap_verifies_shim_not_just_cmd() {
        // 关键断言：cmd-wrap 形态下，shim 路径被送去验签（不只是 cmd.exe）。
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let trust: Arc<dyn crate::trust::TrustPolicy> = Arc::new(RecordingTrust {
            seen: Arc::clone(&seen),
            reject: vec![],
        });
        let host = host_with_trust(trust);
        let shim = abs_path("test-fake-shim.cmd");
        host.spawn(cmd_wrap_req("p1", &shim)).unwrap();

        let seen = seen.lock().unwrap();
        assert!(
            seen.iter().any(|p| p.eq_ignore_ascii_case(&shim)),
            "shim 路径应被验签，实际 verify 调用: {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|p| p.eq_ignore_ascii_case(&abs_path("cmd.exe"))),
            "cmd.exe 也应被验签（先验 program）: {seen:?}"
        );
    }

    #[test]
    fn spawn_cmd_wrap_rejects_unpinned_shim_fail_closed() {
        // shim 未 pin → fail-closed 拒绝，错误指向 shim 路径（驱动 pin UI）。
        let shim = abs_path("test-fake-shim.cmd");
        let trust: Arc<dyn crate::trust::TrustPolicy> = Arc::new(RecordingTrust {
            seen: Arc::new(StdMutex::new(Vec::new())),
            reject: vec![shim.clone()],
        });
        let host = host_with_trust(trust);
        let err = host.spawn(cmd_wrap_req("p1", &shim)).unwrap_err();
        match err {
            ConmuxError::UntrustedProgram { program, reason } => {
                assert!(
                    program.eq_ignore_ascii_case(&shim),
                    "错误应指向 shim 路径，实际: {program}"
                );
                assert!(reason.contains("mock reject"), "reason: {reason}");
            }
            other => panic!("期望 UntrustedProgram，得到 {other:?}"),
        }
    }

    #[test]
    fn spawn_cmd_wrap_relative_arg_not_verified() {
        // `cmd /c dir`（相对 args[1]）→ shim 不触发二次验签，spawn 放行（不破默认 cmd 用法）。
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let trust: Arc<dyn crate::trust::TrustPolicy> = Arc::new(RecordingTrust {
            seen: Arc::clone(&seen),
            reject: vec!["dir".into()],
        });
        let host = host_with_trust(trust);
        let mut r = cmd_wrap_req("p1", "ignored");
        r.command.args = vec!["/c".into(), "dir".into()];
        host.spawn(r).unwrap();
        let seen = seen.lock().unwrap();
        assert!(
            !seen.iter().any(|p| p == "dir"),
            "相对路径 args[1] 不应被验签: {seen:?}"
        );
    }

    // ===== C-2 锁纪律：表锁=短临界区，阻塞操作一律句柄取出后锁外执行 =====

    /// 可控阻塞门：mock 在临界路径上 enter_and_wait 停住，测试线程 wait_entered
    /// 确认到位后做断言，open 放行。
    #[derive(Default)]
    struct Gate {
        inner: StdMutex<GateState>,
        cv: std::sync::Condvar,
    }
    #[derive(Default)]
    struct GateState {
        entered: bool,
        open: bool,
    }
    impl Gate {
        fn enter_and_wait(&self) {
            let mut g = self.inner.lock().unwrap();
            g.entered = true;
            self.cv.notify_all();
            while !g.open {
                g = self.cv.wait(g).unwrap();
            }
        }
        fn wait_entered(&self, timeout: std::time::Duration) -> bool {
            let g = self.inner.lock().unwrap();
            let (_g, r) = self
                .cv
                .wait_timeout_while(g, timeout, |s| !s.entered)
                .unwrap();
            !r.timed_out()
        }
        fn open(&self) {
            let mut g = self.inner.lock().unwrap();
            g.open = true;
            self.cv.notify_all();
        }
    }

    /// 在独立线程跑 op，超时未完成返回 None（探测"被表锁冻结"而不挂死测试进程）。
    fn completes_within<T: Send + 'static>(
        timeout: std::time::Duration,
        op: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(op());
        });
        rx.recv_timeout(timeout).ok()
    }

    const FREEZE_PROBE: std::time::Duration = std::time::Duration::from_secs(2);
    const GATE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

    /// C-2 核心场景：某 pane 的 session 写阻塞（ConPTY 节流）时，resize 同 pane 不得
    /// 持表锁等 session 锁——否则 list/spawn/kill 全部冻结、逃生通道被堵。
    #[test]
    fn blocked_write_with_concurrent_resize_does_not_freeze_host() {
        let Fixture {
            host,
            session_state,
            ..
        } = fixture_with_pid(1);
        let host = Arc::new(host);
        host.spawn(req("a")).unwrap();

        let gate = Arc::new(Gate::default());
        session_state.lock().unwrap().write_gate = Some(Arc::clone(&gate));

        // T1：inject——mock write_all 持 session 锁停在 gate（模拟长阻塞写）。
        let h1 = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || {
                let _ = host.inject_stdin(&PaneId("a".into()), b"x", InjectionSource::UserDirect);
            })
        };
        assert!(gate.wait_entered(GATE_WAIT), "T1 未进入 write_all");

        // T2：resize 同 pane——修复后应在表锁外等 session 锁。
        let h2 = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || {
                let _ = host.resize(&PaneId("a".into()), PaneSize { rows: 30, cols: 100 });
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(100));

        {
            let host = Arc::clone(&host);
            assert!(
                completes_within(FREEZE_PROBE, move || host.list_panes()).is_some(),
                "list_panes 被冻结：resize 在表锁内等 session 锁（C-2）"
            );
        }
        {
            let host = Arc::clone(&host);
            assert!(
                completes_within(FREEZE_PROBE, move || host.spawn(req("b"))).is_some(),
                "spawn 被冻结（C-2）"
            );
        }
        {
            let host = Arc::clone(&host);
            assert!(
                completes_within(FREEZE_PROBE, move || host.kill(&PaneId("a".into()))).is_some(),
                "kill 逃生通道被堵（C-2）"
            );
        }

        gate.open();
        h1.join().unwrap();
        h2.join().unwrap();
    }

    /// 同场景的 poll_exit 变体：session 忙时 poll_exit 不得冻结表。
    #[test]
    fn blocked_write_with_concurrent_poll_exit_does_not_freeze_host() {
        let Fixture {
            host,
            session_state,
            ..
        } = fixture_with_pid(1);
        let host = Arc::new(host);
        host.spawn(req("a")).unwrap();

        let gate = Arc::new(Gate::default());
        session_state.lock().unwrap().write_gate = Some(Arc::clone(&gate));

        let h1 = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || {
                let _ = host.inject_stdin(&PaneId("a".into()), b"x", InjectionSource::UserDirect);
            })
        };
        assert!(gate.wait_entered(GATE_WAIT), "T1 未进入 write_all");

        // poll_exit 自身限时完成（契约 4.3.1）：session 忙 ⇒ try_lock 让路，
        // 返回"本轮不可判定"（Ok(None)），不卡轮询方。
        {
            let host = Arc::clone(&host);
            let polled = completes_within(FREEZE_PROBE, move || {
                host.poll_exit(&PaneId("a".into()))
            });
            assert!(
                matches!(polled, Some(Ok(None))),
                "poll_exit 被忙 session 卡住或误报退出：{polled:?}"
            );
        }
        {
            let host = Arc::clone(&host);
            assert!(
                completes_within(FREEZE_PROBE, move || host.list_panes()).is_some(),
                "list_panes 被冻结：poll_exit 在表锁内等 session 锁（C-2）"
            );
        }

        gate.open();
        h1.join().unwrap();
    }

    /// 代际守卫（与读线程 weak 守卫同源的语义，poll_exit 路径）：句柄取出后、写回前
    /// pane 被 respawn——旧代际的迟到退出码不得污染同 id 新 pane。
    #[test]
    fn poll_exit_late_result_does_not_pollute_respawned_pane() {
        let Fixture {
            host,
            session_state,
            ..
        } = fixture_with_pid(7);
        let host = Arc::new(host);
        host.spawn(req("a")).unwrap();

        let gate = Arc::new(Gate::default());
        {
            let mut s = session_state.lock().unwrap();
            s.exit_gate = Some(Arc::clone(&gate));
            s.exit_code = Some(7);
        }

        // T1：poll_exit——取出旧代际句柄后停在 try_exit_code。
        let h1 = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || host.poll_exit(&PaneId("a".into())))
        };
        assert!(gate.wait_entered(GATE_WAIT), "T1 未进入 try_exit_code");

        // 新代际不需要 gate（mock state 共享，先清掉）。
        session_state.lock().unwrap().exit_gate = None;

        // 同 id respawn → 新代际。修复前：T1 持表锁停在 try_exit_code ⇒ respawn 冻结。
        {
            let host = Arc::clone(&host);
            assert!(
                completes_within(FREEZE_PROBE, move || host.respawn(&PaneId("a".into()), req("a")))
                    .is_some(),
                "respawn 被冻结：poll_exit 在表锁内等 session（C-2）"
            );
        }

        gate.open();
        let res = h1.join().unwrap();
        // 迟到结果按当前代际语义返回 None；新 pane 不得被标 Exited。
        assert_eq!(res.unwrap(), None, "旧代际退出码泄漏给调用方");
        let st = host.pane_state(&PaneId("a".into())).unwrap();
        assert_eq!(
            st.lifecycle,
            PaneLifecycle::Running,
            "旧代际退出码污染了 respawn 后的新 pane（C-2 代际守卫）"
        );
        assert_eq!(st.exit_code, None);
    }

    /// respawn 的 kill_tree 必须在表锁外执行（if-let 临时 guard 延寿陷阱）。
    #[test]
    fn respawn_kill_tree_runs_outside_table_lock() {
        let Fixture { host, sup_rec, .. } = fixture_with_pid(1);
        let host = Arc::new(host);
        host.spawn(req("a")).unwrap();

        let gate = Arc::new(Gate::default());
        sup_rec.lock().unwrap().kill_gate = Some(Arc::clone(&gate));

        let h1 = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || host.respawn(&PaneId("a".into()), req("a")))
        };
        assert!(gate.wait_entered(GATE_WAIT), "T1 未进入 kill_tree");

        {
            let host = Arc::clone(&host);
            assert!(
                completes_within(FREEZE_PROBE, move || host.list_panes()).is_some(),
                "list_panes 被冻结：respawn 的 kill_tree 在表锁内执行（C-2）"
            );
        }

        // 放行前清掉 gate，避免 respawn 内第二次 kill_tree（不存在）或后续清理受扰。
        sup_rec.lock().unwrap().kill_gate = None;
        gate.open();
        h1.join().unwrap().unwrap();
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
            trust: None, // mock 测试不校验信任
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

    // ===== C-2 锁纪律扩展测试（方案 A：锁内重活移出锁外）=====

    /// 辅助：从 host 表里取指定 pane 的 scrollback Arc（测试用，同模块可访问私有字段）。
    fn scrollback_arc(host: &PaneHost, id: &PaneId) -> Arc<Mutex<LineIndexedBuffer>> {
        let panes = host.panes.lock().unwrap();
        Arc::clone(&panes.get(id).unwrap().scrollback)
    }

    /// 测试 1（红绿驱动）：capture 期间表锁不被持有——即使 scrollback 读被阻塞，
    /// 并发 spawn 仍应立即完成（修复前 capture 在表锁内取 scrollback 锁，会冻结全表）。
    #[test]
    fn capture_does_not_freeze_host_during_slow_scrollback_read() {
        let f = fixture_with_pid(1);
        let host = Arc::new(f.host);
        host.spawn(req("a")).unwrap();

        let sb = scrollback_arc(&host, &PaneId("a".into()));
        sb.lock().unwrap().append_and_seq(b"hello\n");

        // T0：持有 scrollback 锁，让 capture 卡在锁外读 scrollback 阶段。
        let sb_guard = sb.lock().unwrap();

        // T1：capture "a"——取句柄（表锁短持）后，卡在 scrollback.lock()。
        let h1 = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || {
                host.capture(crate::capture::CaptureRequest {
                    pane_id: PaneId("a".into()),
                    range: crate::capture::CaptureRange::All,
                    ansi: true,
                })
            })
        };
        // 等 T1 进入 capture 并卡在 scrollback lock。
        std::thread::sleep(std::time::Duration::from_millis(150));

        // T2：spawn "b"——修复后表锁未被 capture 持有，应立即完成。
        let h2 = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || host.spawn(req("b")))
        };
        let result = h2.join().expect("T2 panic");
        assert!(result.is_ok(), "spawn 'b' 不应被 capture 阻塞");

        // 清理：释放 scrollback 锁，让 T1 完成。
        drop(sb_guard);
        let _ = h1.join().expect("T1 panic");
    }

    /// 测试 2（红绿驱动）：list_panes 期间表锁不被持有——即使 scrollback 读被阻塞，
    /// 并发 spawn 仍应立即完成（修复前 list_panes 在表锁内逐 pane 取 scrollback 锁）。
    #[test]
    fn list_panes_does_not_freeze_host_during_slow_scrollback_read() {
        let f = fixture_with_pid(1);
        let host = Arc::new(f.host);
        host.spawn(req("a")).unwrap();

        let sb = scrollback_arc(&host, &PaneId("a".into()));
        sb.lock().unwrap().append_and_seq(b"data\n");

        // T0：持有 scrollback 锁，让 list_panes 卡在锁外读 scrollback 阶段。
        let sb_guard = sb.lock().unwrap();

        // T1：list_panes——表锁短持（拷贝标量 + 取句柄）后，卡在 build_pane_state 的 scrollback lock。
        let h1 = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || host.list_panes())
        };
        std::thread::sleep(std::time::Duration::from_millis(150));

        // T2：spawn "b"——修复后表锁未被 list_panes 持有，应立即完成。
        let h2 = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || host.spawn(req("b")))
        };
        let result = h2.join().expect("T2 panic");
        assert!(result.is_ok(), "spawn 'b' 不应被 list_panes 阻塞");

        drop(sb_guard);
        let _ = h1.join().expect("T1 panic");
    }

    /// 测试 3（代际一致）：capture 取句柄后、读 scrollback 前，pane 被 kill+respawn
    /// （同 id 新 pane）。Arc 引用计数保证读到旧 pane 的 scrollback，不读新 pane、不 panic。
    #[test]
    fn capture_after_respawn_reads_old_generation_scrollback() {
        use base64::Engine;
        let f = fixture_with_pid(1);
        let host = Arc::new(f.host);
        host.spawn(req("a")).unwrap();

        // 旧 pane 写 "old data"。
        let old_sb = scrollback_arc(&host, &PaneId("a".into()));
        old_sb.lock().unwrap().append_and_seq(b"old data\n");

        // T0：持有旧 scrollback 锁，让 capture 卡在锁外读阶段。
        let sb_guard = old_sb.lock().unwrap();

        // T1：capture "a"——取到旧 pane 的 scrollback Arc，卡在 scrollback.lock()。
        let h1 = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || {
                host.capture(crate::capture::CaptureRequest {
                    pane_id: PaneId("a".into()),
                    range: crate::capture::CaptureRange::All,
                    ansi: true,
                })
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(150));

        // kill "a"（remove 旧 pane）+ respawn "a"（新 pane，新 scrollback Arc）。
        host.kill(&PaneId("a".into())).unwrap();
        host.spawn(req("a")).unwrap();

        // 新 pane 写 "new data"。
        let new_sb = scrollback_arc(&host, &PaneId("a".into()));
        new_sb.lock().unwrap().append_and_seq(b"new data\n");

        // 释放旧 scrollback 锁——T1 的 capture 继续读旧 scrollback（Arc 存活）。
        drop(sb_guard);
        let result = h1.join().expect("T1 panic").expect("capture err");

        // 验证读到的是旧 pane 的 "old data"，不是新 pane 的 "new data"。
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&result.data_base64)
            .unwrap();
        assert_eq!(decoded, b"old data\n", "应读到旧代际 scrollback（Arc 存活保证）");
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

        // Slice 1：内核 spawn 守卫要求绝对路径。ConPTY 集成测试用 SystemRoot 拼系统 exe
        // 绝对路径（Windows 标准路径，真机稳定）。
        fn system_cmd() -> String {
            std::path::PathBuf::from(
                std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()),
            )
            .join("System32\\cmd.exe")
            .to_string_lossy()
            .into_owned()
        }
        fn system_powershell() -> String {
            std::path::PathBuf::from(
                std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()),
            )
            .join("System32\\WindowsPowerShell\\v1.0\\powershell.exe")
            .to_string_lossy()
            .into_owned()
        }

        fn win_req(id: &str, echo: &str) -> SpawnRequest {
            SpawnRequest {
                pane_id: PaneId(id.into()),
                command: CommandSpec {
                    program: system_cmd(),
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
                    program: system_cmd(),
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

        /// mode_preamble（M2 重放）：真实 ConPTY 进程发 `?1049h?25l` 停在 alt-screen
        /// → 读线程喂 ModeTracker → 前导含两模式位；kill 后 PaneNotFound。
        #[test]
        fn new_windows_mode_preamble_tracks_alt_screen() {
            let events = Arc::new(Mutex::new(Vec::new()));
            let host = PaneHost::new_windows(
                vec![],
                Arc::new(CollectSink {
                    events: Arc::clone(&events),
                }),
            );
            let req = SpawnRequest {
                pane_id: PaneId("wm".into()),
                command: CommandSpec {
                    program: system_powershell(),
                    args: vec![
                        "-NoProfile".into(),
                        "-Command".into(),
                        // 进 alt-screen + 隐藏光标后驻留（模拟 TUI 运行中）
                        "Write-Host ([char]27+'[?1049h'+[char]27+'[?25l') -NoNewline; Start-Sleep -Seconds 8"
                            .into(),
                    ],
                    cwd: None,
                    env: vec![],
                },
                size: PaneSize { rows: 24, cols: 80 },
                adapter_id: "shell".into(),
                display_name: None,
                created_at: 0,
            };
            host.spawn(req).unwrap();

            // 轮询等模式位被跟踪到（ConPTY 改写不影响 DECSET 透传，spike 实证）。
            let pane_id = PaneId("wm".into());
            let deadline = std::time::Instant::now() + Duration::from_secs(6);
            let preamble = loop {
                let p = host.mode_preamble(&pane_id).expect("pane 应存在");
                if !p.is_empty() || std::time::Instant::now() > deadline {
                    break p;
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            let s = String::from_utf8_lossy(&preamble);
            assert!(s.contains("[?1049h"), "前导应含 alt-screen 位，实际: {s:?}");
            assert!(s.contains("[?25l"), "前导应含光标隐藏位，实际: {s:?}");

            host.kill(&pane_id).unwrap();
            assert!(matches!(
                host.mode_preamble(&pane_id),
                Err(ConmuxError::PaneNotFound { .. })
            ));
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

        /// D-6：attach_snapshot 取回原始 VT 历史 + last_seq>0，且 last_seq 与收到的最后一个
        /// PaneOutput.seq 一致（原子对应）。未知 pane → PaneNotFound。
        #[test]
        fn new_windows_attach_snapshot_history_and_seq() {
            let events = Arc::new(Mutex::new(Vec::new()));
            let host = PaneHost::new_windows(
                vec![],
                Arc::new(CollectSink {
                    events: Arc::clone(&events),
                }),
            );
            host.spawn(win_req("ws", "conmux-attach-marker")).unwrap();
            std::thread::sleep(Duration::from_millis(1800));

            let snap = host
                .attach_snapshot(&PaneId("ws".into()))
                .expect("attach_snapshot 应成功");
            let text = String::from_utf8_lossy(&snap.history);
            assert!(
                text.contains("conmux-attach-marker"),
                "history 应含原始 VT 输出，实际:\n{text}"
            );
            assert!(snap.last_seq > 0, "有输出后 last_seq 应 > 0");
            assert_eq!(snap.pane_state.pane_id, PaneId("ws".into()));

            // last_seq 应等于收到的最后一个 PaneOutput 的 seq（原子对应，无丢无重）。
            let max_emitted_seq = events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    MuxNotify::PaneOutput { seq, .. } => Some(*seq),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            assert_eq!(
                snap.last_seq, max_emitted_seq,
                "快照 last_seq 应与最后 emit 的 PaneOutput.seq 一致（D-6 原子）"
            );

            assert!(matches!(
                host.attach_snapshot(&PaneId("nope".into())),
                Err(ConmuxError::PaneNotFound { .. })
            ));
            host.kill(&PaneId("ws".into())).unwrap();
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
                    program: system_cmd(),
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
