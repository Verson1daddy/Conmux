//! 命名管道传输原语（Windows，M2 设计 D-3 / 契约 §3.1 I-1..I-6）。
//!
//! 仅 Windows 编译（在 lib.rs 经 `#[cfg(windows)]` 挂载）。提供：
//! - [`default_pipe_name`]：`\\.\pipe\conmux.<用户SID>`（I-1 命名）。
//! - [`PipeListener`]：服务端监听器——首实例带 `FILE_FLAG_FIRST_PIPE_INSTANCE`（I-2 防抢注）、
//!   DACL 仅授权当前用户 SID（I-1 实质隔离）、`PIPE_REJECT_REMOTE_CLIENTS`（I-3）、多实例（I-4）。
//! - [`PipeStream`]：单连接字节流（Read+Write+Drop），含客户端身份取数（I-5）。
//! - [`try_connect`]：客户端连接（区分「无 daemon」以驱动自动拉起）。
//!
//! **威胁模型（诚实声明，契约 §3.1 I-2 注）**：同用户恶意进程本就能注入/调试同用户进程；
//! 管道层防冒充意在抬高门槛 + 可审计，不对抗已得手的同用户恶意代码。抢注最坏退化为
//! DoS（daemon 起不来），不产生静默劫持（依赖客户端侧签名/路径校验报警，见 client.rs）。

use std::ffi::c_void;
use std::io::{self, Read, Write};
use std::iter::once;
use std::time::Duration;

use crate::ConmuxError;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE,
    ERROR_FILE_NOT_FOUND, ERROR_HANDLE_EOF, ERROR_INSUFFICIENT_BUFFER, ERROR_IO_PENDING,
    ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER, SECURITY_ATTRIBUTES,
    PSECURITY_DESCRIPTOR,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

const PIPE_PREFIX: &str = r"\\.\pipe\conmux.";
const PIPE_BUF_SIZE: u32 = 64 * 1024;

// ===== 公开入口 =====

/// 当前用户默认管道名 `\\.\pipe\conmux.<SID>`（I-1）。名称可枚举，实质隔离靠 DACL。
pub fn default_pipe_name() -> Result<String, ConmuxError> {
    Ok(format!("{PIPE_PREFIX}{}", current_user_sid_string()?))
}

/// 客户端连接结果——区分「连上」与「无 daemon」（后者驱动自动拉起，client.rs）。
pub enum ConnectOutcome {
    Connected(PipeStream),
    /// 管道不存在（`ERROR_FILE_NOT_FOUND`）——daemon 未运行。
    NoDaemon,
}

/// 客户端连接命名管道。`ERROR_PIPE_BUSY` 时 `WaitNamedPipeW` 等待实例可用再重试
/// （`busy_wait_ms` 单次等待预算）；`ERROR_FILE_NOT_FOUND` ⇒ `NoDaemon`。
pub fn try_connect(name: &str, busy_wait_ms: u32) -> Result<ConnectOutcome, ConmuxError> {
    let wide = to_wide(name);
    // 有限重试：BUSY 时等待后重试，最多 8 轮防活锁。
    for _ in 0..8 {
        // SAFETY: 标准 Win32；无共享、OPEN_EXISTING、FILE_FLAG_OVERLAPPED（客户端 attach 时
        // 也需读写并发——重叠句柄不串行；控制态顺序读写同样适用）。
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            return PipeStream::from_handle(handle).map(ConnectOutcome::Connected);
        }
        let err = unsafe { GetLastError() };
        match err {
            ERROR_FILE_NOT_FOUND => return Ok(ConnectOutcome::NoDaemon),
            ERROR_PIPE_BUSY => {
                // 等一个实例空出（0 = 超时，继续下一轮）。
                unsafe { WaitNamedPipeW(wide.as_ptr(), busy_wait_ms) };
                continue;
            }
            other => {
                return Err(ConmuxError::PtyError {
                    message: format!("连接管道 {name} 失败（GetLastError={other}）"),
                });
            }
        }
    }
    Err(ConmuxError::PtyError {
        message: format!("连接管道 {name} 反复 BUSY，放弃"),
    })
}

// ===== PipeStream：单连接字节流（重叠 I/O，可拆读/写半）=====
//
// **为何重叠 I/O（FILE_FLAG_OVERLAPPED）**：同步（非重叠）句柄上，I/O 管理器**串行化**
// 同一文件对象的全部 I/O——reader 线程阻塞在 ReadFile 时，writer 线程的 WriteFile 会排在
// 其后无法进行（实测死锁：握手 HelloAck 永不发出）。重叠句柄上每个操作各带 OVERLAPPED +
// 事件，读写互不串行——daemon 每连接 reader+writer 双线程方得并发（D-7）。

/// 共享底层句柄（重叠模式）。`PipeStream`/`PipeReader`/`PipeWriter` 各持一个 `Arc`，
/// 最后一个 drop 时关句柄。
struct PipeHandle {
    handle: HANDLE,
}
unsafe impl Send for PipeHandle {}
unsafe impl Sync for PipeHandle {}
impl Drop for PipeHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄由 PipeHandle 单一所有，最后一个 Arc drop 时关一次。
        unsafe { CloseHandle(self.handle) };
    }
}

/// 一个重叠操作流的手动复位事件（每读/写半各一个，使读写并发不串行）。
struct IoEvent {
    event: HANDLE,
}
impl IoEvent {
    fn new() -> Result<Self, ConmuxError> {
        // 手动复位（bManualReset=1）、初始非信号（bInitialState=0）、无名。
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(ConmuxError::PtyError {
                message: format!("CreateEventW 失败（GetLastError={}）", unsafe {
                    GetLastError()
                }),
            });
        }
        Ok(Self { event })
    }
}
unsafe impl Send for IoEvent {}
impl Drop for IoEvent {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.event) };
    }
}

/// 重叠读：发起 ReadFile(OVERLAPPED) → 等完成。对端关闭 ⇒ `Ok(0)`。
///
/// `timeout_ms`：
/// - `None`（默认，流式 reader）：`GetOverlappedResult(bWait=1)` 无限等——长驻 pane 输出
///   稀疏，读半必须无限等否则会把"暂时没输出"误判为错误。
/// - `Some(ms)`（控制连接请求-应答）：仍 pending 时 `WaitForSingleObject(event, ms)` 限时等；
///   超时则 `CancelIoEx` 取消本次读 + 排空被取消操作，返回 `TimedOut`——防 wedged daemon
///   下控制请求无限阻塞持锁（红队 SF-1）。只作用于 `PipeStream`，不波及 `PipeReader`（流式）。
fn overlapped_read(
    handle: HANDLE,
    event: HANDLE,
    buf: &mut [u8],
    timeout_ms: Option<u32>,
) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.hEvent = event;
    // SAFETY: 重叠句柄；buf 有效；lpNumberOfBytesRead=null（重叠下经 GetOverlappedResult 取）。
    let ok = unsafe {
        ReadFile(
            handle,
            buf.as_mut_ptr().cast(),
            buf.len().min(u32::MAX as usize) as u32,
            std::ptr::null_mut(),
            &mut ov,
        )
    };
    let mut pending = false;
    if ok == 0 {
        let err = unsafe { GetLastError() };
        if err == ERROR_IO_PENDING {
            pending = true;
        } else {
            return map_read_err(err);
        }
    }
    // 限时模式且仍在途：限时等事件，未 signaled（超时或 WAIT_FAILED）则取消本次 I/O。
    if pending {
        if let Some(ms) = timeout_ms {
            // SAFETY: event 为本流手动复位事件，ReadFile 完成时由内核置信号。
            let wait = unsafe { WaitForSingleObject(event, ms) };
            if wait != WAIT_OBJECT_0 {
                // 超时 / WAIT_FAILED 一律：先 CancelIoEx 取消在途读，再 bWait=1 排空——确保本帧
                // 返回前 ov/event 已无在途 I/O 引用（LOW-2：闭合 WAIT_FAILED 分支的生命周期洞）。
                // SAFETY: 取消本句柄上以 &ov 标识的在途读；GetOverlappedResult bWait=1 必返
                // （取消的 op 很快以 ABORTED 完成，或它其实已真完成）。
                unsafe { CancelIoEx(handle, &ov) };
                let mut drained: u32 = 0;
                let r = unsafe { GetOverlappedResult(handle, &ov, &mut drained, 1) };
                // SF-1 竞态：读在 cancel 前已真完成 → drained 字节已落 buf，按成功返还（不丢已到帧）。
                if r != 0 && drained > 0 {
                    return Ok(drained as usize);
                }
                if wait == WAIT_TIMEOUT {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "命名管道读超时"));
                }
                // WAIT_FAILED 等（near-impossible）：已取消排空，返错。
                return map_read_err(unsafe { GetLastError() });
            }
            // WAIT_OBJECT_0：已 signaled → 落到下面 bWait=1（已完成，立即返回）取字节。
        }
    }
    let mut got: u32 = 0;
    // bWait=TRUE：等本操作完成（经 ov.hEvent，不串行化其它方向操作）。已完成则立即返回。
    let r = unsafe { GetOverlappedResult(handle, &ov, &mut got, 1) };
    if r != 0 {
        Ok(got as usize)
    } else {
        map_read_err(unsafe { GetLastError() })
    }
}

fn map_read_err(err: u32) -> io::Result<usize> {
    if err == ERROR_BROKEN_PIPE || err == ERROR_NO_DATA || err == ERROR_HANDLE_EOF {
        Ok(0) // 对端关闭 ⇒ EOF
    } else {
        Err(io::Error::from_raw_os_error(err as i32))
    }
}

/// 重叠写：发起 WriteFile(OVERLAPPED) → GetOverlappedResult(bWait) 等完成。
fn overlapped_write(handle: HANDLE, event: HANDLE, buf: &[u8]) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.hEvent = event;
    // SAFETY: 重叠句柄；buf 有效；lpNumberOfBytesWritten=null。
    let ok = unsafe {
        WriteFile(
            handle,
            buf.as_ptr(),
            buf.len().min(u32::MAX as usize) as u32,
            std::ptr::null_mut(),
            &mut ov,
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        if err != ERROR_IO_PENDING {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
    }
    let mut got: u32 = 0;
    let r = unsafe { GetOverlappedResult(handle, &ov, &mut got, 1) };
    if r != 0 {
        Ok(got as usize)
    } else {
        Err(io::Error::from_raw_os_error(unsafe { GetLastError() } as i32))
    }
}

/// 单连接字节流端点（服务端 accept 或客户端 connect 得到）。Read+Write；可 [`split`](PipeStream::split)
/// 为读半/写半交两个线程并发（daemon 每连接 reader+writer 双线程模型，D-7）。
pub struct PipeStream {
    inner: std::sync::Arc<PipeHandle>,
    ev: IoEvent,
    /// 读超时（None=无限等，默认）。仅控制连接（请求-应答）设；流式 reader（attach）不设。
    read_timeout_ms: Option<u32>,
}

impl PipeStream {
    fn from_handle(handle: HANDLE) -> Result<Self, ConmuxError> {
        Ok(Self {
            inner: std::sync::Arc::new(PipeHandle { handle }),
            ev: IoEvent::new()?,
            read_timeout_ms: None,
        })
    }

    /// 设读超时（控制连接用，防 wedged daemon 下请求无限阻塞持锁）。`None`=恢复无限等。
    /// `Duration` 截断为毫秒（饱和到 `u32::MAX`）。不影响写、不影响已 `split` 出的流式读半。
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.read_timeout_ms = timeout.map(|d| {
            let ms = d.as_millis();
            if ms > u32::MAX as u128 {
                u32::MAX
            } else {
                ms as u32
            }
        });
    }

    /// 取客户端进程 id（I-5：身份不可得 ⇒ None，调用方 fail-closed 断连）。
    pub fn client_process_id(&self) -> Option<u32> {
        let mut pid: u32 = 0;
        // SAFETY: 句柄为服务端 accept 得到的管道实例。
        let ok = unsafe { GetNamedPipeClientProcessId(self.inner.handle, &mut pid) };
        if ok != 0 {
            Some(pid)
        } else {
            None
        }
    }

    /// 取**服务端**进程 id（客户端侧反冒充用，I-2 客户端校验）：与服务端 image 比对防被
    /// 抢注者冒充 daemon 收割注入。身份不可得 ⇒ None。
    pub fn server_process_id(&self) -> Option<u32> {
        let mut pid: u32 = 0;
        // SAFETY: 句柄为客户端 CreateFile 得到的管道实例。
        let ok = unsafe { GetNamedPipeServerProcessId(self.inner.handle, &mut pid) };
        if ok != 0 {
            Some(pid)
        } else {
            None
        }
    }

    /// 拆为读半 + 写半（共享句柄，各自独立事件）。reader 半交 reader 线程重叠 ReadFile；
    /// writer 半交 writer 线程从有界队列取帧重叠 WriteFile——两半不同事件 ⇒ 不串行。
    pub fn split(self) -> Result<(PipeReader, PipeWriter), ConmuxError> {
        let reader = PipeReader {
            inner: std::sync::Arc::clone(&self.inner),
            ev: IoEvent::new()?,
        };
        let writer = PipeWriter {
            inner: std::sync::Arc::clone(&self.inner),
            ev: IoEvent::new()?,
        };
        Ok((reader, writer))
    }
}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        overlapped_read(self.inner.handle, self.ev.event, buf, self.read_timeout_ms)
    }
}
impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        overlapped_write(self.inner.handle, self.ev.event, buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 连接读半（只读）。交 daemon 每连接的 reader 线程。
pub struct PipeReader {
    inner: std::sync::Arc<PipeHandle>,
    ev: IoEvent,
}
impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // 流式读半永远无限等（长驻 pane 输出稀疏；不可把"暂无输出"当超时）。
        overlapped_read(self.inner.handle, self.ev.event, buf, None)
    }
}

/// 连接写半（只写）。交 daemon 每连接的 writer 线程（drain 有界外发队列）。
pub struct PipeWriter {
    inner: std::sync::Arc<PipeHandle>,
    ev: IoEvent,
}
impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        overlapped_write(self.inner.handle, self.ev.event, buf)
    }
    /// no-op：WriteFile 已把字节交给管道；背压由 daemon 有界队列治理（D-7）。
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ===== PipeListener：服务端监听 =====

/// 服务端管道监听器。`bind` 即创建首实例（`FILE_FLAG_FIRST_PIPE_INSTANCE` 防抢注，I-2）；
/// `accept` 连接当前 pending 实例并预建下一实例（始终有一个实例在听，减小竞态窗口）。
pub struct PipeListener {
    name_wide: Vec<u16>,
    /// 共享安全描述符（DACL 仅当前用户 SID），CreateNamedPipe 复制进内核对象；本结构 drop 时 LocalFree。
    psd: PSECURITY_DESCRIPTOR,
    /// 下一个待 ConnectNamedPipe 的实例句柄（null = 需重建）。
    pending: HANDLE,
}

// psd/pending 裸指针：listener 单一所有权，accept 串行调用，跨线程移动安全（不共享）。
unsafe impl Send for PipeListener {}

impl PipeListener {
    /// 绑定管道名：派生当前用户 SID 的 DACL，创建带 FIRST_PIPE_INSTANCE 的首实例（I-2 抢注守卫）。
    /// 首实例创建失败（名已存在 = 已有 daemon / 被抢注）⇒ Err，**不降级复用**（契约 I-2）。
    pub fn bind(name: &str) -> Result<Self, ConmuxError> {
        let name_wide = to_wide(name);
        let psd = user_only_security_descriptor()?;
        let pending = match create_instance(&name_wide, psd, true) {
            Ok(h) => h,
            Err(e) => {
                unsafe { LocalFree(psd as *mut c_void) };
                return Err(e);
            }
        };
        Ok(Self {
            name_wide,
            psd,
            pending,
        })
    }

    /// 接受一个连接：ConnectNamedPipe 当前 pending（阻塞至客户端连入）→ 预建下一实例 → 返回流。
    pub fn accept(&mut self) -> Result<PipeStream, ConmuxError> {
        // 保证入口有 pending 实例（上次预建失败时为 null）。
        if self.pending == INVALID_HANDLE_VALUE || self.pending.is_null() {
            self.pending = create_instance(&self.name_wide, self.psd, false)?;
        }
        let handle = self.pending;
        // 重叠句柄上 ConnectNamedPipe 须带 OVERLAPPED：用临时事件等客户端连入。
        let conn_ev = IoEvent::new()?;
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = conn_ev.event;
        // SAFETY: handle 为本 listener 创建的重叠管道实例；ov 栈上有效到 GetOverlappedResult 返回。
        let ok = unsafe { ConnectNamedPipe(handle, &mut ov) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            match err {
                // 等客户端连入（GetOverlappedResult bWait 经 ov.hEvent 等待）。
                ERROR_IO_PENDING => {
                    let mut got: u32 = 0;
                    let r = unsafe { GetOverlappedResult(handle, &ov, &mut got, 1) };
                    if r == 0 {
                        let e2 = unsafe { GetLastError() };
                        unsafe { CloseHandle(handle) };
                        self.pending = INVALID_HANDLE_VALUE;
                        return Err(ConmuxError::PtyError {
                            message: format!("ConnectNamedPipe 等待失败（GetLastError={e2}）"),
                        });
                    }
                }
                // 客户端在 CreateNamedPipe 与 ConnectNamedPipe 之间已连入 ⇒ 成功。
                ERROR_PIPE_CONNECTED => {}
                other => {
                    unsafe { CloseHandle(handle) };
                    self.pending = INVALID_HANDLE_VALUE;
                    return Err(ConmuxError::PtyError {
                        message: format!("ConnectNamedPipe 失败（GetLastError={other}）"),
                    });
                }
            }
        }
        drop(conn_ev);
        // 预建下一实例（best-effort：失败则置 null，下次 accept 入口重建）。
        self.pending = create_instance(&self.name_wide, self.psd, false)
            .unwrap_or(INVALID_HANDLE_VALUE);
        // from_handle 失败（事件创建失败，极罕见）时 Arc<PipeHandle> drop 会关 handle。
        PipeStream::from_handle(handle)
    }
}

impl Drop for PipeListener {
    fn drop(&mut self) {
        // SAFETY: pending（若有）+ psd 由本结构单一所有。
        if self.pending != INVALID_HANDLE_VALUE && !self.pending.is_null() {
            unsafe { CloseHandle(self.pending) };
        }
        unsafe { LocalFree(self.psd as *mut c_void) };
    }
}

/// 创建一个命名管道实例。`first=true` 带 `FILE_FLAG_FIRST_PIPE_INSTANCE`（抢注守卫，仅 daemon 启动首个）。
fn create_instance(
    name_wide: &[u16],
    psd: PSECURITY_DESCRIPTOR,
    first: bool,
) -> Result<HANDLE, ConmuxError> {
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd,
        bInheritHandle: 0, // FALSE：句柄不可被子进程继承
    };
    // FILE_FLAG_OVERLAPPED：重叠模式——使 daemon 每连接 reader/writer 双线程读写不串行（防死锁）。
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // SAFETY: name_wide 以 null 结尾；sa 栈上有效，CreateNamedPipe 复制 SD 进内核对象。
    let handle = unsafe {
        CreateNamedPipeW(
            name_wide.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, // I-3
            PIPE_UNLIMITED_INSTANCES, // I-4
            PIPE_BUF_SIZE,
            PIPE_BUF_SIZE,
            0,
            &mut sa,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let err = unsafe { GetLastError() };
        let hint = if first && err == ERROR_ACCESS_DENIED {
            "（FIRST_PIPE_INSTANCE：管道名已被占用——已有 daemon 或被抢注；拒绝降级，I-2）"
        } else {
            ""
        };
        return Err(ConmuxError::PtyError {
            message: format!("CreateNamedPipeW 失败（GetLastError={err}）{hint}"),
        });
    }
    Ok(handle)
}

// ===== 身份与安全描述符 FFI =====

/// 给定 pid 取进程映像全路径（I-5 连接级审计：客户端身份记录）。失败 ⇒ None。
pub fn process_image_path(pid: u32) -> Option<String> {
    // SAFETY: QUERY_LIMITED_INFORMATION 仅查询，bInheritHandle=FALSE。
    let proc = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if proc.is_null() {
        return None;
    }
    let mut buf = [0u16; 1024];
    let mut size = buf.len() as u32;
    // 第二参 0 = PROCESS_NAME_WIN32（Win32 路径格式）。
    let ok = unsafe { QueryFullProcessImageNameW(proc, 0, buf.as_mut_ptr(), &mut size) };
    unsafe { CloseHandle(proc) };
    if ok != 0 {
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    } else {
        None
    }
}

/// 当前进程用户的 SID 字符串（如 `S-1-5-21-...`）。
fn current_user_sid_string() -> Result<String, ConmuxError> {
    // SAFETY: 标准 token 查询序列。
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(token_err("OpenProcessToken"));
        }
        // 取所需缓冲大小（首调期望失败 + ERROR_INSUFFICIENT_BUFFER）。
        let mut len: u32 = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        let last = GetLastError();
        if len == 0 || last != ERROR_INSUFFICIENT_BUFFER {
            CloseHandle(token);
            return Err(token_err("GetTokenInformation(size)"));
        }
        let mut buf = vec![0u8; len as usize];
        if GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
            CloseHandle(token);
            return Err(token_err("GetTokenInformation"));
        }
        CloseHandle(token);

        let token_user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut psid_str: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(token_user.User.Sid, &mut psid_str) == 0 {
            return Err(token_err("ConvertSidToStringSidW"));
        }
        let s = wide_ptr_to_string(psid_str);
        LocalFree(psid_str as *mut c_void);
        Ok(s)
    }
}

/// 构造仅授权当前用户 SID 的安全描述符（DACL，I-1 实质隔离）。
/// 返回的 PSECURITY_DESCRIPTOR 由调用方在用完后 `LocalFree`。
fn user_only_security_descriptor() -> Result<PSECURITY_DESCRIPTOR, ConmuxError> {
    let sid = current_user_sid_string()?;
    // D:P(A;;GA;;;<SID>) —— DACL 受保护(P)、仅一条 Allow Generic-All ACE 给当前用户；
    // 未列出者（Everyone/Network/SYSTEM）一律无访问。
    let sddl = format!("D:P(A;;GA;;;{sid})");
    let sddl_wide = to_wide(&sddl);
    let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: sddl_wide 以 null 结尾；psd 为 out 参数，成功后需 LocalFree。
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1 as u32,
            &mut psd,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(ConmuxError::PtyError {
            message: format!("构造 DACL 安全描述符失败（GetLastError={err}）"),
        });
    }
    Ok(psd)
}

fn token_err(stage: &str) -> ConmuxError {
    let err = unsafe { GetLastError() };
    ConmuxError::PtyError {
        message: format!("{stage} 失败（GetLastError={err}）"),
    }
}

// ===== 宽字符串助手 =====

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(once(0)).collect()
}

/// 读 null 结尾宽字符串（用于 ConvertSidToStringSidW 输出）。
fn wide_ptr_to_string(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    // SAFETY: p 指向 null 结尾宽串（Win32 分配）。
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(p, len);
        String::from_utf16_lossy(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 服务端-客户端真实管道往返 + 客户端身份取数（I-5）。
    #[test]
    fn server_client_roundtrip_and_client_pid() {
        let name = r"\\.\pipe\conmux-test-roundtrip";
        let mut listener = PipeListener::bind(name).expect("bind 应成功");

        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().expect("accept 应成功");
            // 身份可得（I-5）——客户端 = 本测试进程。
            let pid = stream.client_process_id().expect("应能取客户端 pid");
            assert_eq!(pid, std::process::id(), "同进程客户端 pid 应为本进程");
            // echo：读一帧字节回写。
            let mut buf = [0u8; 5];
            stream.read_exact(&mut buf).expect("server 读");
            stream.write_all(&buf).expect("server 回写");
            stream.flush().ok();
            // 保持流存活到客户端读完。
            let mut tail = [0u8; 1];
            let _ = stream.read(&mut tail);
        });

        // 客户端连接（等服务端就绪）。
        let mut client = loop {
            match try_connect(name, 500).expect("try_connect") {
                ConnectOutcome::Connected(s) => break s,
                ConnectOutcome::NoDaemon => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        };
        client.write_all(b"hello").unwrap();
        client.flush().unwrap();
        let mut got = [0u8; 5];
        client.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"hello");
        drop(client);
        server.join().unwrap();
    }

    /// I-2 抢注守卫：同名第二次 bind（FIRST_PIPE_INSTANCE）必失败、不降级。
    #[test]
    fn first_instance_flag_blocks_squatting() {
        let name = r"\\.\pipe\conmux-test-squat";
        let _first = PipeListener::bind(name).expect("首个 bind 成功");
        // 第二个 bind 同名 ⇒ FIRST_PIPE_INSTANCE 拒绝（已有实例）。
        let second = PipeListener::bind(name);
        assert!(
            second.is_err(),
            "同名第二次 bind 必须失败（I-2 防抢注，不降级复用）"
        );
    }

    /// 无服务端时连接 ⇒ NoDaemon（驱动自动拉起）。
    #[test]
    fn connect_without_daemon_reports_no_daemon() {
        let name = r"\\.\pipe\conmux-test-absent-daemon";
        match try_connect(name, 50).expect("try_connect 不应硬错") {
            ConnectOutcome::NoDaemon => {}
            ConnectOutcome::Connected(_) => panic!("不应连上不存在的 daemon"),
        }
    }

    /// 读超时（SF-1）：服务端延迟写 → 客户端短超时首读返 TimedOut；之后流仍可读到数据
    /// （证 CancelIoEx 未弄坏句柄）。
    #[test]
    fn read_timeout_returns_timedout_then_stream_recovers() {
        let name = r"\\.\pipe\conmux-test-read-timeout";
        let mut listener = PipeListener::bind(name).expect("bind 应成功");

        let server = std::thread::spawn(move || {
            let mut s = listener.accept().expect("accept 应成功");
            // 故意延迟 400ms 才写——制造客户端首读超时。
            std::thread::sleep(Duration::from_millis(400));
            s.write_all(b"late!").expect("server 写");
            s.flush().ok();
            // 保活到客户端读完。
            let mut tail = [0u8; 1];
            let _ = s.read(&mut tail);
        });

        let mut client = loop {
            match try_connect(name, 500).expect("try_connect") {
                ConnectOutcome::Connected(s) => break s,
                ConnectOutcome::NoDaemon => std::thread::sleep(Duration::from_millis(20)),
            }
        };

        // 首读：120ms 超时，服务端 400ms 才写 → 应 TimedOut 且在超时附近返回（非无限阻塞）。
        client.set_read_timeout(Some(Duration::from_millis(120)));
        let mut buf = [0u8; 5];
        let started = std::time::Instant::now();
        let r = client.read(&mut buf);
        let elapsed = started.elapsed();
        assert!(
            matches!(&r, Err(e) if e.kind() == io::ErrorKind::TimedOut),
            "首读应 TimedOut，实际 {r:?}"
        );
        assert!(
            elapsed < Duration::from_millis(350),
            "应在超时附近返回（非无限阻塞），实际 {elapsed:?}"
        );

        // 恢复：放宽超时，流仍可读到服务端后续写出的数据（CancelIoEx 未弄坏句柄）。
        client.set_read_timeout(Some(Duration::from_millis(2000)));
        let mut got = Vec::new();
        let mut tmp = [0u8; 8];
        while got.len() < 5 {
            match client.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&tmp[..n]),
                Err(e) => panic!("恢复读失败: {e:?}"),
            }
        }
        assert_eq!(&got, b"late!", "取消后流应仍能读到后续数据");
        drop(client);
        server.join().unwrap();
    }

    /// SID 字符串派生可用，默认管道名含前缀。
    #[test]
    fn default_pipe_name_contains_sid() {
        let name = default_pipe_name().expect("派生默认管道名");
        assert!(name.starts_with(PIPE_PREFIX), "前缀: {name}");
        assert!(name.contains("S-1-"), "应含 SID 字符串: {name}");
    }
}
