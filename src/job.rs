//! 进程监管抽象（API 契约 §3 / MF-4）。
//!
//! 每个 pane 一个监管器（Windows = JobObject）。本模块定义 trait 与工厂；
//! Windows `JobObjectSupervisor` 真实实现（windows-sys + KILL_ON_JOB_CLOSE +
//! fail-closed 四条款 + 禁 BREAKAWAY）在系统集成子步（V0-1/V0-5）落地。

use crate::ConmuxError;

/// 单个 pane 的进程监管器。fail-closed 四条款语义见契约 §3.1（实现侧保证）。
pub trait ProcessSupervisor: Send + Sync {
    /// 将 pid 纳入监管（Windows = AssignProcessToJobObject）。
    fn assign(&self, pid: u32) -> Result<(), ConmuxError>;
    /// 整树终结（Windows = TerminateJobObject）。
    fn kill_tree(&self) -> Result<(), ConmuxError>;
}

/// 监管器工厂：PaneHost 每次 spawn 创建一个新监管器（每 pane 一个 Job）。
///
/// 抽象成工厂而非具体类型，使 PaneHost 不绑定 Windows、便于 mock 测试机制层。
pub trait SupervisorFactory: Send + Sync {
    fn create(&self) -> Box<dyn ProcessSupervisor>;
}

// ===== Windows JobObject 实现（cutover 2b-1 / MF-4 / V0-1 / V0-5）=====
//
// 仅 Windows 编译——conmux 纯逻辑层（types/scrollback/capture/pane mock）保持跨平台可测。
#[cfg(windows)]
mod windows_impl {
    use super::{ProcessSupervisor, SupervisorFactory};
    use crate::ConmuxError;
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, TerminateJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// 每 pane 一个 Windows Job Object。fail-closed 四条款 + KILL_ON_JOB_CLOSE +
    /// 禁 BREAKAWAY（契约 §3.1 / 复闸 C4）。
    ///
    /// **生命周期语义（关窗即丢，契约 §2 / D4）**：job 句柄设 KILL_ON_JOB_CLOSE，
    /// 最后一个句柄关闭（= 本结构体 drop）即整树终结全部成员进程。故 Pane 私有持有
    /// 本监管器、随 Pane drop 释放 = app 退出/崩溃零孤儿。
    pub struct JobObjectSupervisor {
        job: HANDLE,
    }

    // HANDLE 是裸指针，非 Send/Sync。本句柄单一所有权（仅本结构体持有）、所有操作
    // （assign/kill_tree/drop）在 PaneHost 的锁下串行，故跨线程移动/共享安全。
    unsafe impl Send for JobObjectSupervisor {}
    unsafe impl Sync for JobObjectSupervisor {}

    impl JobObjectSupervisor {
        /// 创建一个 Job：null 安全属性 ⇒ **句柄不可继承**（bInheritHandle=FALSE，契约 cl.3）；
        /// 设 KILL_ON_JOB_CLOSE；**不设** BREAKAWAY_OK / SILENT_BREAKAWAY_OK（cl.5 / C4）。
        pub fn new() -> Result<Self, ConmuxError> {
            // SAFETY: 标准 Win32 调用，null 名/属性 = 匿名不可继承 job。
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(ConmuxError::SupervisorError {
                    message: "CreateJobObjectW 失败".into(),
                });
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION =
                unsafe { std::mem::zeroed() };
            // 仅设 KILL_ON_JOB_CLOSE——不含任何 BREAKAWAY 标志（整树监管不可被静默废除）。
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: info 是栈上已初始化结构，长度匹配。
            let ok = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                unsafe { CloseHandle(job) };
                return Err(ConmuxError::SupervisorError {
                    message: "SetInformationJobObject(KILL_ON_JOB_CLOSE) 失败".into(),
                });
            }
            Ok(Self { job })
        }
    }

    impl ProcessSupervisor for JobObjectSupervisor {
        fn assign(&self, pid: u32) -> Result<(), ConmuxError> {
            // AssignProcessToJobObject 需 PROCESS_SET_QUOTA | PROCESS_TERMINATE 访问权。
            // SAFETY: 标准 Win32；bInheritHandle=FALSE(0)。
            let proc = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
            if proc.is_null() {
                return Err(ConmuxError::SupervisorError {
                    message: format!("OpenProcess(pid={pid}) 失败（进程不存在/无权限）"),
                });
            }
            let ok = unsafe { AssignProcessToJobObject(self.job, proc) };
            // 进程句柄用完即关（job 已持有成员引用，不需保留此句柄）。
            unsafe { CloseHandle(proc) };
            if ok == 0 {
                return Err(ConmuxError::SupervisorError {
                    message: format!("AssignProcessToJobObject(pid={pid}) 失败"),
                });
            }
            Ok(())
        }

        fn kill_tree(&self) -> Result<(), ConmuxError> {
            // SAFETY: 终结 job 内全部进程（含子孙树）。退出码 1。
            let ok = unsafe { TerminateJobObject(self.job, 1) };
            if ok == 0 {
                return Err(ConmuxError::SupervisorError {
                    message: "TerminateJobObject 失败".into(),
                });
            }
            Ok(())
        }
    }

    impl Drop for JobObjectSupervisor {
        fn drop(&mut self) {
            // 关闭最后一个 job 句柄 ⇒ KILL_ON_JOB_CLOSE 触发整树终结（关窗即丢）。
            // SAFETY: job 由本结构体单一所有，drop 时关闭一次。
            unsafe { CloseHandle(self.job) };
        }
    }

    /// 生产工厂：每次 spawn 创建一个新 JobObjectSupervisor。
    pub struct JobObjectSupervisorFactory;

    impl SupervisorFactory for JobObjectSupervisorFactory {
        fn create(&self) -> Box<dyn ProcessSupervisor> {
            // new() 失败时退化为一个永远 fail 的监管器，使 PaneHost::spawn 在 assign
            // 阶段 fail-closed（不产生无监管 pane）。实践中 CreateJobObjectW 几乎不失败。
            match JobObjectSupervisor::new() {
                Ok(s) => Box::new(s),
                Err(_) => Box::new(FailedSupervisor),
            }
        }
    }

    /// Job 创建失败时的占位——assign 必失败 ⇒ spawn fail-closed。
    struct FailedSupervisor;
    impl ProcessSupervisor for FailedSupervisor {
        fn assign(&self, _pid: u32) -> Result<(), ConmuxError> {
            Err(ConmuxError::SupervisorError {
                message: "JobObject 创建失败，拒绝监管（fail-closed）".into(),
            })
        }
        fn kill_tree(&self) -> Result<(), ConmuxError> {
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        fn spawn_ping() -> std::process::Child {
            Command::new("cmd")
                .args(["/c", "ping -t 127.0.0.1"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .spawn()
                .expect("spawn cmd/ping 应成功")
        }

        /// 轮询等进程退出（kill 是异步的），超时返回 false。
        fn wait_exit(child: &mut std::process::Child, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            loop {
                if let Ok(Some(_)) = child.try_wait() {
                    return true;
                }
                if Instant::now() > deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        #[test]
        fn new_creates_job() {
            assert!(JobObjectSupervisor::new().is_ok());
        }

        #[test]
        fn assign_then_kill_tree_terminates_process() {
            let sup = JobObjectSupervisor::new().unwrap();
            let mut child = spawn_ping();
            sup.assign(child.id()).expect("assign 真实 pid 应成功");
            assert!(child.try_wait().unwrap().is_none(), "kill 前应在运行");
            sup.kill_tree().expect("kill_tree 应成功");
            assert!(
                wait_exit(&mut child, Duration::from_secs(5)),
                "kill_tree 后进程应终结"
            );
            let _ = child.kill(); // 兜底
        }

        #[test]
        fn dropping_supervisor_kills_via_kill_on_job_close() {
            // 关窗即丢（KILL_ON_JOB_CLOSE）：监管器 drop ⇒ 整树终结。
            let mut child = spawn_ping();
            {
                let sup = JobObjectSupervisor::new().unwrap();
                sup.assign(child.id()).unwrap();
                // sup 在此 drop ⇒ job 句柄关闭 ⇒ KILL_ON_JOB_CLOSE 杀进程。
            }
            assert!(
                wait_exit(&mut child, Duration::from_secs(5)),
                "drop 监管器后进程应被 KILL_ON_JOB_CLOSE 终结"
            );
            let _ = child.kill();
        }

        #[test]
        fn assign_nonexistent_pid_returns_err() {
            let sup = JobObjectSupervisor::new().unwrap();
            // pid 0 / 极大 pid → OpenProcess 失败 → fail-closed Err。
            assert!(matches!(
                sup.assign(0xFFFF_FFF0),
                Err(ConmuxError::SupervisorError { .. })
            ));
        }

        #[test]
        fn factory_creates_working_supervisor() {
            let f = JobObjectSupervisorFactory;
            let sup = f.create();
            let mut child = spawn_ping();
            sup.assign(child.id()).expect("工厂监管器 assign 应成功");
            sup.kill_tree().unwrap();
            assert!(wait_exit(&mut child, Duration::from_secs(5)));
            let _ = child.kill();
        }
    }
}

#[cfg(windows)]
pub use windows_impl::{JobObjectSupervisor, JobObjectSupervisorFactory};
