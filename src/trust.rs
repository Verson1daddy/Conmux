//! # 信任校验（Slice 2 · 安全本体）
//!
//! 对到达 `PaneHost::spawn` 的**已解析绝对路径** program 做三档信任决策：
//!
//! - **A 档**：WinVerifyTrust Authenticode 通过 + 签名主体命中受信 publisher 列表 → 放行。
//! - **B 档**：无签名 / 验签失败，但 (绝对路径, SHA-256) 命中信任库 pinned_targets → 放行。
//! - **C 档**：其余 → fail-closed 拒绝。
//!
//! ## 架构
//! - `TrustPolicy` trait：注入 `PaneHost`（参照 hooks/event_sink 模式），spawn 热路径不做
//!   文件 I/O——TrustStore 启动时加载一次。
//! - 纯决策逻辑（`decide`）与 FFI（`win_verify_and_get_publisher`）分离，便于 mock 单测。
//! - FFI 两段调用：`WTD_STATEACTION_VERIFY` 验签 → 取签名主体 → `WTD_STATEACTION_CLOSE`
//!   释放状态（防句柄泄漏 / UAF）。
//!
//! ## 威胁模型与边界（诚实声明）
//! 本闸是**完整性闸**：保证被 spawn 的二进制是「微软签名」**或**「用户已 pin（路径+哈希）」，
//! 用于挡住「往 PATH/启动项塞一个被替换/伪造的未签名 CLI」这类攻击。它**不是 per-CLI 白名单**。
//!
//! - **A 档语义**：publisher O= **精确匹配**受信组织（种子 "Microsoft Corporation"）。精确匹配
//!   只防 "Microsoft Corporation Evil" 这类前缀冒名——**不收窄到具体程序**。
//! - **B 档语义**：无签名目标按**绝对路径 + 内容 SHA-256**绑定，shim 被替换即失配 → 拒。
//!   （路径/哈希绑定**仅 B 档**有；A 档纯凭签名，不绑路径/哈希。）
//! - mode 默认 **enforce**（fail-closed）；warn = 记日志仍放行；off = 跳过（仅调试）。
//!
//! ## 已知限制（红队 2026-06-20 实证，方案 A 接受为已知风险）
//! 1. **A 档信任锚为组织级**：O=Microsoft Corporation 覆盖**全部**微软签名二进制，含 LOLBins
//!    （mshta/regsvr32/rundll32/wscript/cscript/certutil/InstallUtil…）。即本闸允许任意微软签名
//!    二进制——利用前提是攻击者已能控制启动配置（届时其本可直接 pin 自己的马）。若要收窄到
//!    具体程序白名单（方案 B），见后续硬化项。
//! 2. **不查吊销**（`WTD_REVOKE_NONE`）：被吊销证书签的文件仍过 A 档（权衡：离线不误杀）。
//! 3. **TOCTOU 残留**：verify 与真正 spawn 之间存在文件被替换的窗口；Slice 1 绝对路径守卫
//!    只消除「裸名 PATH 解析歧义」，**未**消除该替换窗口。
//! 4. **mode 完整性**：trust.toml 在用户可写目录，任何用户级进程可改 mode=off 关闭本闸。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ===== 公开类型 =====

/// 信任校验模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustMode {
    /// fail-closed：C 档拒绝（默认）。
    Enforce,
    /// 记日志但仍放行（调试 / 回滚安全阀）。
    Warn,
    /// 跳过校验（仅调试）。
    Off,
}

impl Default for TrustMode {
    fn default() -> Self {
        Self::Enforce
    }
}

/// 哈希钉条目：绝对路径 + SHA-256 绑定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedTarget {
    pub path: String,
    pub sha256: String,
}

/// 信任库（持久化到 `%APPDATA%\conmux\trust.toml`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustStore {
    pub mode: TrustMode,
    pub trusted_publishers: Vec<String>,
    pub pinned_targets: Vec<PinnedTarget>,
}

/// 信任决策（`TrustPolicy::verify` 返回值）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustDecision {
    /// 放行。
    Allow,
    /// 拒绝（含原因）。
    Reject { reason: String },
}

/// 信任策略注入点（参照 `InjectionHook` / `PaneEventSink` 模式）。
/// 实现侧（`TrustStore`）启动时加载一次；spawn 热路径不做文件 I/O。
pub trait TrustPolicy: Send + Sync {
    /// 对已解析的绝对路径 program 做信任校验，返回决策。
    fn verify(&self, program: &Path) -> TrustDecision;
}

/// 线程安全的共享信任库（`Arc<SharedTrustStore>` 同时注入 PaneHost + Tauri State）。
///
/// `Mutex<TrustStore>` 内部可变：pin/unpin 即时生效，下次 verify 即看到新条目。
/// verify 持锁期间做 FFI + 文件 I/O（SHA-256），但桌面应用 spawn 频率低，可接受。
pub struct SharedTrustStore {
    inner: std::sync::Mutex<TrustStore>,
}

impl SharedTrustStore {
    /// 从文件加载（或用种子默认创建）并包装为共享态。
    pub fn load_or_create() -> Self {
        Self {
            inner: std::sync::Mutex::new(TrustStore::load_or_create()),
        }
    }

    /// 快照（clone 当前 TrustStore，用于 trust_list 命令）。
    pub fn snapshot(&self) -> TrustStore {
        self.inner.lock().unwrap().clone()
    }

    /// pin 一个可执行文件（算 SHA-256 + 写 pinned_targets + 存盘）。
    pub fn pin_executable(&self, path: &str) -> Result<(), String> {
        self.inner.lock().unwrap().pin_executable(path)
    }

    /// 移除 pin（存盘）。
    pub fn unpin(&self, path: &str) -> Result<(), String> {
        self.inner.lock().unwrap().unpin(path)
    }
}

impl TrustPolicy for SharedTrustStore {
    fn verify(&self, program: &Path) -> TrustDecision {
        self.inner.lock().unwrap().verify(program)
    }
}

// ===== TrustStore 默认值 + 持久化 =====

/// 种子受信 publisher（让 powershell.exe / cmd.exe / wsl.exe 走 A 档）。
const SEED_TRUSTED_PUBLISHERS: &[&str] = &["Microsoft Corporation"];

impl Default for TrustStore {
    fn default() -> Self {
        Self {
            mode: TrustMode::Enforce,
            trusted_publishers: SEED_TRUSTED_PUBLISHERS.iter().map(|s| (*s).to_string()).collect(),
            pinned_targets: Vec::new(),
        }
    }
}

impl TrustStore {
    /// 信任库文件路径：`%APPDATA%\conmux\trust.toml`。
    /// 非 Windows / 无 APPDATA → 返回 None（仅内存态，不持久化）。
    fn file_path() -> Option<PathBuf> {
        let appdata = std::env::var("APPDATA").ok()?;
        Some(PathBuf::from(appdata).join("conmux").join("trust.toml"))
    }

    /// 从文件加载；缺文件 → 用种子默认 + 写出；解析失败 → fail-closed（enforce 下视为
    /// 空信任库，不静默放行——仅种子 publisher 生效，pinned 全清）。
    pub fn load_or_create() -> Self {
        let path = match Self::file_path() {
            Some(p) => p,
            None => return Self::default(), // 非 Windows / 无 APPDATA：仅内存态。
        };

        match std::fs::read_to_string(&path) {
            Ok(content) => match toml::from_str::<TrustStore>(&content) {
                Ok(store) => store,
                Err(e) => {
                    // 解析失败 → fail-closed：用默认（种子 publisher + 空 pin），不静默放行。
                    // 不覆盖坏文件（让用户手动修）；下次启动仍读坏文件 → 仍 fail-closed。
                    eprintln!(
                        "[conmux/trust] trust.toml 解析失败，回退默认（fail-closed）: {e}"
                    );
                    Self::default()
                }
            },
            Err(_) => {
                // 缺文件 → 用种子默认 + 写出（best-effort，失败不阻塞）。
                let store = Self::default();
                if let Err(e) = store.save() {
                    eprintln!("[conmux/trust] 写出默认 trust.toml 失败（不阻塞）: {e}");
                }
                store
            }
        }
    }

    /// 存盘（best-effort）。
    pub fn save(&self) -> Result<(), String> {
        let path = Self::file_path().ok_or_else(|| "无 APPDATA，无法持久化".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建 trust 目录失败: {e}"))?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| format!("序列化 trust.toml 失败: {e}"))?;
        std::fs::write(&path, content).map_err(|e| format!("写 trust.toml 失败: {e}"))
    }

    /// pin 一个可执行文件：算 SHA-256 + 加入 pinned_targets + 存盘。
    pub fn pin_executable(&mut self, path: &str) -> Result<(), String> {
        // 校验绝对路径：与 PaneHost::spawn 守卫同口径。B 档按 path 串精确比较，相对路径 pin
        // 进库后永不命中 spawn 侧的绝对路径——污染信任库且无效，故 fail-fast 拒绝。
        if !Path::new(path).is_absolute() {
            return Err(format!("pin 路径必须是绝对路径: {path}"));
        }
        let hash = compute_sha256(Path::new(path))
            .map_err(|e| format!("计算 SHA-256 失败: {e}"))?;
        // 去重：同路径覆盖旧哈希（shim 更新后重新 pin）。
        if let Some(existing) = self.pinned_targets.iter_mut().find(|t| t.path == path) {
            existing.sha256 = hash;
        } else {
            self.pinned_targets.push(PinnedTarget {
                path: path.to_string(),
                sha256: hash,
            });
        }
        self.save()
    }

    /// 移除 pin。
    pub fn unpin(&mut self, path: &str) -> Result<(), String> {
        self.pinned_targets.retain(|t| t.path != path);
        self.save()
    }
}

// ===== 纯决策逻辑（无 FFI，可单测）=====

/// 纯决策函数：给定签名主体（FFI 结果）+ 信任库，返回决策。
///
/// - `signed_publisher = Some(name)`：WinVerifyTrust 通过，取到签名主体。
/// - `signed_publisher = None`：无签名 / 验签失败 / FFI 不可用。
/// - `file_sha256`：文件实际 SHA-256（B 档比对用；None = 算不出，直接 C 档）。
///
/// 此函数不处理 mode（off/warn/enforce）——mode 由 `TrustStore::verify` 包裹。
/// 此函数始终返回真实决策（enforce 语义），调用方按 mode 决定是否放行。
pub fn decide(
    program: &Path,
    signed_publisher: Option<&str>,
    file_sha256: Option<&str>,
    store: &TrustStore,
) -> TrustDecision {
    // A 档：签名有效 + publisher 精确匹配。
    if let Some(pub_name) = signed_publisher {
        // 精确匹配（非 contains / 前缀），防 "Microsoft Corporation Evil" 冒名。
        if store
            .trusted_publishers
            .iter()
            .any(|p| p == pub_name)
        {
            return TrustDecision::Allow;
        }
        return TrustDecision::Reject {
            reason: format!(
                "签名有效但主体不在受信列表: {pub_name}（program: {}）",
                program.display()
            ),
        };
    }

    // B 档：无签名 → 哈希钉（路径 + 内容绑定）。
    if let Some(hash) = file_sha256 {
        let path_str = program.to_string_lossy();
        if store
            .pinned_targets
            .iter()
            .any(|t| t.path == path_str && t.sha256 == hash)
        {
            return TrustDecision::Allow;
        }
        return TrustDecision::Reject {
            reason: format!(
                "无签名且未 pin（path={}, sha256={}）",
                path_str, hash
            ),
        };
    }

    // C 档：无签名 + 算不出哈希（文件不存在 / I/O 错误）→ fail-closed。
    TrustDecision::Reject {
        reason: format!(
            "无签名且无法计算哈希（文件不存在 / I/O 错误）: {}",
            program.display()
        ),
    }
}

// ===== TrustPolicy 实现 =====

impl TrustPolicy for TrustStore {
    fn verify(&self, program: &Path) -> TrustDecision {
        match self.mode {
            TrustMode::Off => TrustDecision::Allow,
            TrustMode::Warn => {
                let decision = self.verify_enforce(program);
                if matches!(decision, TrustDecision::Reject { .. }) {
                    eprintln!(
                        "[conmux/trust] WARN 模式放行（不拒绝）: {}",
                        match &decision {
                            TrustDecision::Reject { reason } => reason.as_str(),
                            _ => "",
                        }
                    );
                }
                TrustDecision::Allow
            }
            TrustMode::Enforce => self.verify_enforce(program),
        }
    }
}

impl TrustStore {
    /// enforce 语义的真实决策（不含 mode 包裹）。
    fn verify_enforce(&self, program: &Path) -> TrustDecision {
        // FFI 验签（仅 Windows；非 Windows 返 None → 走 B/C 档）。
        let signed_publisher = win_verify_and_get_publisher(program);
        // 算 SHA-256（B 档比对；文件不存在返 None → C 档）。
        let file_sha256 = compute_sha256(program).ok();
        decide(
            program,
            signed_publisher.as_deref(),
            file_sha256.as_deref(),
            self,
        )
    }
}

// ===== SHA-256（手写纯 Rust，FIPS 180-4，零依赖）=====
//
// 不用 `sha2` crate：其依赖 `generic-array` 的 build script 调 `version_check`（spawn
// `rustc`），在沙箱环境里 `CreateProcess` 返回 code 0 但进程未启动 → build script panic。
// 手写实现避免该问题，且 SHA-256 算法稳定、可单测验证。

/// SHA-256 轮常量（FIPS 180-4 §4.2.2）。
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 初始哈希值（FIPS 180-4 §5.3.3）。
const SHA256_H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// SHA-256 哈希器（增量式）。
struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            h: SHA256_H0,
            buf: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.total_len += data.len() as u64;
        let mut data = data;
        // 先填满 buffer。
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = need.min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        // 整块处理。
        while data.len() >= 64 {
            let block: [u8; 64] = data[..64].try_into().unwrap();
            self.compress(&block);
            data = &data[64..];
        }
        // 剩余存 buffer。
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        // 填充：0x80 + 0x00... + 8 字节大端长度。
        let bit_len = self.total_len * 8;
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;
        if self.buf_len > 56 {
            for i in self.buf_len..64 {
                self.buf[i] = 0;
            }
            let block = self.buf;
            self.compress(&block);
            self.buf_len = 0;
        }
        for i in self.buf_len..56 {
            self.buf[i] = 0;
        }
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        self.compress(&block);

        let mut out = [0u8; 32];
        for (i, &word) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
        self.h[5] = self.h[5].wrapping_add(f);
        self.h[6] = self.h[6].wrapping_add(g);
        self.h[7] = self.h[7].wrapping_add(h);
    }
}

/// 算文件 SHA-256，返回小写十六进制字符串。
fn compute_sha256(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("打开文件失败: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf).map_err(|e| format!("读文件失败: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hash = hasher.finalize();
    Ok(hash.iter().map(|b| format!("{:02x}", b)).collect())
}

// ===== WinVerifyTrust FFI（仅 Windows）=====

/// WinVerifyTrust Authenticode 验签 + 取签名主体（**O= 组织字段**）。
///
/// **两段调用模式**（嵌入 + 目录均遵循）：
/// 1. `WTD_STATEACTION_VERIFY`：验签 + 持有状态（供 WTHelper 链读取签名主体）。
/// 2. `WTD_STATEACTION_CLOSE`：释放状态（**必须执行**，防句柄泄漏 / UAF）。
///
/// **目录签名回退**（真机 bug 修复）：cmd.exe/powershell.exe/wsl.exe 是 catalog-signed
/// （SignatureType=Catalog，签名在系统 .cat 文件，非嵌入 PE）。文件模式（WTD_CHOICE_FILE）
/// WinVerifyTrust 对这些文件返回 TRUST_E_NOSIGNATURE → 回退目录验证：
/// `CryptCATAdminAcquireContext2` → 算文件哈希 → `CryptCATAdminEnumCatalogFromHash` 找
/// catalog → `CryptCATCatalogInfoFromContext` 取 catalog 路径 → `WTD_CHOICE_CATALOG` 再验。
///
/// 返回 `Some(publisher)` = 验签通过 + 取到签名主体 O= 字段；
/// `None` = 无签名 / 验签失败 / FFI 不可用（走 B/C 档）。
///
/// **假设**（catalog 路径真机未验，沙箱无 ConPTY/真签名 exe）：
/// - `CryptCATAdminAcquireContext2` 的 `pgsubsystem` 传 NULL（默认子系统）——MSDN 说 NULL
///   等价默认，对 Windows 系统 catalog 应足够；若真机发现需 `SUBSYSTEM_WINDOWS` GUID
///   （{832D1A4E-BC8C-4E0F-9DE3-E0B6C3E7B4F4}，windows-sys 0.59 未导出），后续补。
/// - `pcwszMemberTag` 传哈希 hex 字符串（MSDN 惯例）。
#[cfg(windows)]
fn win_verify_and_get_publisher(path: &Path) -> Option<String> {
    // 先试嵌入签名。
    if let Some(pub_name) = verify_embedded_signature(path) {
        return Some(pub_name);
    }
    // 嵌入签名无 / 验签失败 → 回退目录签名。
    verify_catalog_signature(path)
}

/// 嵌入签名验证（WTD_CHOICE_FILE）。
#[cfg(windows)]
fn verify_embedded_signature(path: &Path) -> Option<String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::WinTrust::*;

    // 路径 → UTF-16（null 终止）。
    let wide: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // WINTRUST_FILE_INFO。
    let mut file_info: WINTRUST_FILE_INFO = unsafe { std::mem::zeroed() };
    file_info.cbStruct = std::mem::size_of::<WINTRUST_FILE_INFO>() as u32;
    file_info.pcwszFilePath = wide.as_ptr();
    // hFile = NULL（让 WinVerifyTrust 自己开文件）。

    // WINTRUST_DATA。
    let mut wt_data: WINTRUST_DATA = unsafe { std::mem::zeroed() };
    wt_data.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
    wt_data.dwUIChoice = WTD_UI_NONE;
    wt_data.fdwRevocationChecks = WTD_REVOKE_NONE; // 关吊销检查（审计 §4 M6：离线误杀）。
    wt_data.dwUnionChoice = WTD_CHOICE_FILE;
    // union 字段写入。windows-sys 0.59 字段名为 `Anonymous`。
    wt_data.Anonymous.pFile = &mut file_info;
    wt_data.dwStateAction = WTD_STATEACTION_VERIFY; // 阶段 1：验签 + 持状态。

    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;

    // 阶段 1：验签。返回 0 = 通过。
    let result = unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            &mut wt_data as *mut WINTRUST_DATA as *mut core::ffi::c_void,
        )
    };

    let publisher = if result == 0 {
        extract_publisher(&wt_data)
    } else {
        None
    };

    // 阶段 2：释放状态（**必须执行**，即使阶段 1 失败）。
    wt_data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            &mut wt_data as *mut WINTRUST_DATA as *mut core::ffi::c_void,
        );
    }

    publisher
}

/// 目录签名验证（WTD_CHOICE_CATALOG，回退路径）。
///
/// 流程：`CryptCATAdminAcquireContext2` → 开文件句柄 →
/// `CryptCATAdminCalcHashFromFileHandle2`（两段：先拿大小，再填入）→
/// `CryptCATAdminEnumCatalogFromHash`（null = 真无签名 → None）→
/// `CryptCATCatalogInfoFromContext` 取 catalog 路径 →
/// `WINTRUST_CATALOG_INFO` + `WTD_CHOICE_CATALOG` 再 WinVerifyTrust →
/// 验通过 → extract_publisher。
///
/// 清理（即使中途失败也执行）：`WTD_STATEACTION_CLOSE` +
/// `CryptCATAdminReleaseCatalogContext` + `CryptCATAdminReleaseContext` + 文件句柄 drop。
#[cfg(windows)]
fn verify_catalog_signature(path: &Path) -> Option<String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
    use windows_sys::Win32::Security::Cryptography::Catalog::*;
    use windows_sys::Win32::Security::WinTrust::*;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, OPEN_EXISTING,
    };

    // 路径 → UTF-16（null 终止），供 pcwszMemberFilePath 用。
    let wide_path: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // 1. CryptCATAdminAcquireContext2（pgsubsystem = NULL，默认子系统）。
    let mut h_cat_admin: isize = 0;
    let acquired = unsafe {
        CryptCATAdminAcquireContext2(
            &mut h_cat_admin,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
        )
    };
    if acquired == 0 {
        return None;
    }

    // 2. 开文件句柄（GENERIC_READ + OPEN_EXISTING）。
    let h_file: HANDLE = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(), // hTemplateFile = NULL
        )
    };
    // h_file == INVALID_HANDLE_VALUE (-1) = 失败。
    if h_file as isize == -1 {
        unsafe { CryptCATAdminReleaseContext(h_cat_admin, 0) };
        return None;
    }

    // 3. 算文件哈希（两段：先拿大小，再填入）。
    let mut cb_hash: u32 = 0;
    let ok = unsafe {
        CryptCATAdminCalcHashFromFileHandle2(h_cat_admin, h_file, &mut cb_hash, std::ptr::null_mut(), 0)
    };
    if ok == 0 || cb_hash == 0 {
        unsafe {
            CloseHandle(h_file);
            CryptCATAdminReleaseContext(h_cat_admin, 0);
        }
        return None;
    }
    let mut hash_buf = vec![0u8; cb_hash as usize];
    let ok = unsafe {
        CryptCATAdminCalcHashFromFileHandle2(
            h_cat_admin,
            h_file,
            &mut cb_hash,
            hash_buf.as_mut_ptr(),
            0,
        )
    };
    if ok == 0 {
        unsafe {
            CloseHandle(h_file);
            CryptCATAdminReleaseContext(h_cat_admin, 0);
        }
        return None;
    }

    // 4. CryptCATAdminEnumCatalogFromHash（null = 真无 catalog → None）。
    let mut h_cat_info: isize = 0;
    let cat_ctx = unsafe {
        CryptCATAdminEnumCatalogFromHash(h_cat_admin, hash_buf.as_ptr(), cb_hash, 0, &mut h_cat_info)
    };
    if cat_ctx == 0 {
        // 真无签名（无 catalog）→ 清理 + None。
        unsafe {
            CloseHandle(h_file);
            CryptCATAdminReleaseContext(h_cat_admin, 0);
        }
        return None;
    }

    // 5. CryptCATCatalogInfoFromContext 取 catalog 文件路径。
    let mut cat_info: CATALOG_INFO = unsafe { std::mem::zeroed() };
    cat_info.cbStruct = std::mem::size_of::<CATALOG_INFO>() as u32;
    let ok = unsafe { CryptCATCatalogInfoFromContext(cat_ctx, &mut cat_info, 0) };
    if ok == 0 {
        unsafe {
            CryptCATAdminReleaseCatalogContext(h_cat_admin, cat_ctx, 0);
            CloseHandle(h_file);
            CryptCATAdminReleaseContext(h_cat_admin, 0);
        }
        return None;
    }

    // 6. pcwszMemberTag = 哈希 hex 字符串（UTF-16 null 终止）。
    let tag_hex: String = hash_buf.iter().map(|b| format!("{:02X}", b)).collect();
    let mut wide_tag: Vec<u16> = OsStr::new(&tag_hex)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // 7. 组 WINTRUST_CATALOG_INFO + WINTRUST_DATA(WTD_CHOICE_CATALOG)。
    let mut cat_info_wt: WINTRUST_CATALOG_INFO = unsafe { std::mem::zeroed() };
    cat_info_wt.cbStruct = std::mem::size_of::<WINTRUST_CATALOG_INFO>() as u32;
    cat_info_wt.pcwszCatalogFilePath = cat_info.wszCatalogFile.as_ptr();
    cat_info_wt.pcwszMemberFilePath = wide_path.as_ptr();
    cat_info_wt.pcwszMemberTag = wide_tag.as_mut_ptr();
    cat_info_wt.hMemberFile = h_file;
    cat_info_wt.pbCalculatedFileHash = hash_buf.as_mut_ptr();
    cat_info_wt.cbCalculatedFileHash = cb_hash;
    cat_info_wt.hCatAdmin = h_cat_admin;

    let mut wt_data: WINTRUST_DATA = unsafe { std::mem::zeroed() };
    wt_data.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
    wt_data.dwUIChoice = WTD_UI_NONE;
    wt_data.fdwRevocationChecks = WTD_REVOKE_NONE;
    wt_data.dwUnionChoice = WTD_CHOICE_CATALOG;
    wt_data.Anonymous.pCatalog = &mut cat_info_wt;
    wt_data.dwStateAction = WTD_STATEACTION_VERIFY;

    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;

    // 阶段 1：验签。
    let result = unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            &mut wt_data as *mut WINTRUST_DATA as *mut core::ffi::c_void,
        )
    };

    let publisher = if result == 0 {
        extract_publisher(&wt_data)
    } else {
        None
    };

    // 阶段 2：释放状态（**必须执行**）。
    wt_data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            &mut wt_data as *mut WINTRUST_DATA as *mut core::ffi::c_void,
        );
    }

    // 清理 catalog + catadmin + 文件句柄。
    unsafe {
        CryptCATAdminReleaseCatalogContext(h_cat_admin, cat_ctx, 0);
        CryptCATAdminReleaseContext(h_cat_admin, 0);
        CloseHandle(h_file);
    }

    publisher
}

/// 从 WinVerifyTrust 状态数据提取签名主体名称（**O= 组织字段**）。
///
/// 链路：`WTHelperProvDataFromStateData(hWVTStateData)` →
/// `WTHelperGetProvSignerFromChain` → `WTHelperGetProvCertFromChain` →
/// `CertGetNameStringW`（`CERT_NAME_ATTR_TYPE` = 3 + `pvTypePara = szOID_ORGANIZATION_NAME`
/// = "2.5.4.10"，取 O= 组织名，如 "Microsoft Corporation"）。
///
/// **为何取 O= 而非 CN**：cmd.exe/powershell.exe 等系统文件签名主体 subject 为
/// "CN=Microsoft Windows, O=Microsoft Corporation, ..."——CN 是 "Microsoft Windows"
/// （与种子 "Microsoft Corporation" 不匹配），O= 才是 "Microsoft Corporation"。
/// 嵌入签名与目录签名共用此提取逻辑。
///
/// 证书上下文释放：`WTHelperGetProvCertFromChain` 返回的 `CRYPT_PROVIDER_CERT.pCert`
/// 是 borrowed（由 WinTrust state 持有），不需调 `CertFreeCertificateContext`——
/// state close 时统一释放。
#[cfg(windows)]
fn extract_publisher(
    wt_data: &windows_sys::Win32::Security::WinTrust::WINTRUST_DATA,
) -> Option<String> {
    use windows_sys::Win32::Security::Cryptography::{
        CertGetNameStringW, CERT_NAME_ATTR_TYPE, szOID_ORGANIZATION_NAME,
    };
    use windows_sys::Win32::Security::WinTrust::*;

    unsafe {
        // 从 WINTRUST_DATA.hWVTStateData 取 CRYPT_PROVIDER_DATA。
        let prov_data = WTHelperProvDataFromStateData(wt_data.hWVTStateData);
        if prov_data.is_null() {
            return None;
        }

        // 取第一个签名者。fCounterSigner = FALSE(0)。
        let signer = WTHelperGetProvSignerFromChain(prov_data, 0, 0, 0);
        if signer.is_null() {
            return None;
        }

        // 取签名者证书。
        let prov_cert = WTHelperGetProvCertFromChain(signer, 0);
        if prov_cert.is_null() {
            return None;
        }

        // CRYPT_PROVIDER_CERT.pCert → PCCERT_CONTEXT。
        let cert_ctx = (*prov_cert).pCert;
        if cert_ctx.is_null() {
            return None;
        }

        // 取主体 O= 字段（CERT_NAME_ATTR_TYPE = 3 + szOID_ORGANIZATION_NAME = "2.5.4.10"）。
        // pvTypePara 传 OID 字符串指针（PCSTR = *const u8 → *const c_void）。
        let mut name_buf = [0u16; 512];
        let len = CertGetNameStringW(
            cert_ctx,
            CERT_NAME_ATTR_TYPE,
            0,
            szOID_ORGANIZATION_NAME as *const core::ffi::c_void,
            name_buf.as_mut_ptr(),
            name_buf.len() as u32,
        );

        if len <= 1 {
            // len == 1 表示空字符串（仅 null 终止）。
            return None;
        }

        // len 含 null 终止，实际字符数 = len - 1。
        let name = String::from_utf16_lossy(&name_buf[..len as usize - 1]);

        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }
}

#[cfg(not(windows))]
fn win_verify_and_get_publisher(_path: &Path) -> Option<String> {
    None
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_publishers(publishers: &[&str]) -> TrustStore {
        TrustStore {
            mode: TrustMode::Enforce,
            trusted_publishers: publishers.iter().map(|s| s.to_string()).collect(),
            pinned_targets: Vec::new(),
        }
    }

    // A 档：签名有效 + publisher 精确匹配 → Allow。
    #[test]
    fn decide_a_signed_trusted_publisher_allows() {
        let store = store_with_publishers(&["Microsoft Corporation"]);
        let decision = decide(
            Path::new("C:\\Windows\\System32\\cmd.exe"),
            Some("Microsoft Corporation"),
            None,
            &store,
        );
        assert_eq!(decision, TrustDecision::Allow);
    }

    // A 档反例：签名有效但 publisher 不在列表 → Reject。
    #[test]
    fn decide_a_signed_untrusted_publisher_rejects() {
        let store = store_with_publishers(&["Microsoft Corporation"]);
        let decision = decide(
            Path::new("C:\\evil\\malware.exe"),
            Some("Evil Corp"),
            None,
            &store,
        );
        assert!(matches!(decision, TrustDecision::Reject { .. }));
    }

    // 精确匹配防冒名："Microsoft Corporation Evil" 不匹配 "Microsoft Corporation"。
    #[test]
    fn decide_publisher_exact_match_prevents_prefix_spoofing() {
        let store = store_with_publishers(&["Microsoft Corporation"]);
        let decision = decide(
            Path::new("C:\\evil\\spoof.exe"),
            Some("Microsoft Corporation Evil"),
            None,
            &store,
        );
        assert!(
            matches!(decision, TrustDecision::Reject { .. }),
            "前缀冒名应被拒（精确匹配），实际: {decision:?}"
        );
    }

    // B 档：无签名 + 路径+哈希命中 pin → Allow。
    #[test]
    fn decide_b_unsigned_pinned_allows() {
        let mut store = store_with_publishers(&["Microsoft Corporation"]);
        store.pinned_targets.push(PinnedTarget {
            path: "C:\\Users\\test\\claude.cmd".to_string(),
            sha256: "abc123".to_string(),
        });
        let decision = decide(
            Path::new("C:\\Users\\test\\claude.cmd"),
            None,
            Some("abc123"),
            &store,
        );
        assert_eq!(decision, TrustDecision::Allow);
    }

    // B 档反例：无签名 + 哈希不符 → Reject（shim 被替换即失配）。
    #[test]
    fn decide_b_unsigned_hash_mismatch_rejects() {
        let mut store = store_with_publishers(&["Microsoft Corporation"]);
        store.pinned_targets.push(PinnedTarget {
            path: "C:\\Users\\test\\claude.cmd".to_string(),
            sha256: "abc123".to_string(),
        });
        let decision = decide(
            Path::new("C:\\Users\\test\\claude.cmd"),
            None,
            Some("different_hash"),
            &store,
        );
        assert!(
            matches!(decision, TrustDecision::Reject { .. }),
            "哈希不符应拒，实际: {decision:?}"
        );
    }

    // B 档反例：无签名 + 路径不符（同哈希不同路径）→ Reject。
    #[test]
    fn decide_b_unsigned_path_mismatch_rejects() {
        let mut store = store_with_publishers(&["Microsoft Corporation"]);
        store.pinned_targets.push(PinnedTarget {
            path: "C:\\Users\\test\\claude.cmd".to_string(),
            sha256: "abc123".to_string(),
        });
        let decision = decide(
            Path::new("C:\\Users\\test\\evil.cmd"),
            None,
            Some("abc123"),
            &store,
        );
        assert!(
            matches!(decision, TrustDecision::Reject { .. }),
            "路径不符应拒（路径绑定），实际: {decision:?}"
        );
    }

    // C 档：无签名 + 无 pin + 有哈希 → Reject。
    #[test]
    fn decide_c_unsigned_unpinned_rejects() {
        let store = store_with_publishers(&["Microsoft Corporation"]);
        let decision = decide(
            Path::new("C:\\Users\\test\\unknown.cmd"),
            None,
            Some("some_hash"),
            &store,
        );
        assert!(matches!(decision, TrustDecision::Reject { .. }));
    }

    // C 档：无签名 + 算不出哈希 → Reject。
    #[test]
    fn decide_c_no_hash_rejects() {
        let store = store_with_publishers(&["Microsoft Corporation"]);
        let decision = decide(
            Path::new("C:\\nonexistent\\foo.exe"),
            None,
            None,
            &store,
        );
        assert!(matches!(decision, TrustDecision::Reject { .. }));
    }

    // mode=off → 始终 Allow（安全阀）。
    #[test]
    fn trust_store_off_mode_always_allows() {
        let store = TrustStore {
            mode: TrustMode::Off,
            trusted_publishers: Vec::new(),
            pinned_targets: Vec::new(),
        };
        let decision = store.verify(Path::new("C:\\evil\\anything.exe"));
        assert_eq!(decision, TrustDecision::Allow);
    }

    // mode=warn → 即使 enforce 会拒，仍 Allow（安全阀）。
    #[test]
    fn trust_store_warn_mode_allows_even_if_would_reject() {
        let store = TrustStore {
            mode: TrustMode::Warn,
            trusted_publishers: Vec::new(),
            pinned_targets: Vec::new(),
        };
        // verify_enforce 会因无签名 + 无 pin 拒，但 warn 模式应放行。
        let decision = store.verify(Path::new("C:\\nonexistent\\foo.exe"));
        assert_eq!(decision, TrustDecision::Allow);
    }

    // mode=enforce → 真实决策（C 档拒）。
    #[test]
    fn trust_store_enforce_mode_rejects_untrusted() {
        let store = TrustStore {
            mode: TrustMode::Enforce,
            trusted_publishers: Vec::new(),
            pinned_targets: Vec::new(),
        };
        let decision = store.verify(Path::new("C:\\nonexistent\\foo.exe"));
        assert!(matches!(decision, TrustDecision::Reject { .. }));
    }

    // TrustStore 默认值含种子 publisher。
    #[test]
    fn default_store_has_seed_publishers() {
        let store = TrustStore::default();
        assert!(store.trusted_publishers.contains(&"Microsoft Corporation".to_string()));
        assert_eq!(store.mode, TrustMode::Enforce);
    }

    // TOML 序列化 round-trip。
    #[test]
    fn trust_store_toml_roundtrip() {
        let store = TrustStore {
            mode: TrustMode::Warn,
            trusted_publishers: vec!["Microsoft Corporation".to_string()],
            pinned_targets: vec![PinnedTarget {
                path: "C:\\test\\claude.cmd".to_string(),
                sha256: "abc123".to_string(),
            }],
        };
        let toml_str = toml::to_string(&store).expect("序列化");
        let back: TrustStore = toml::from_str(&toml_str).expect("反序列化");
        assert_eq!(store.mode, back.mode);
        assert_eq!(store.trusted_publishers, back.trusted_publishers);
        assert_eq!(store.pinned_targets, back.pinned_targets);
    }

    // SHA-256 已知答案：空串。
    #[test]
    fn sha256_empty_string() {
        let mut hasher = Sha256::new();
        hasher.update(b"");
        let hash = hasher.finalize();
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    // SHA-256 已知答案："abc"。
    #[test]
    fn sha256_abc() {
        let mut hasher = Sha256::new();
        hasher.update(b"abc");
        let hash = hasher.finalize();
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // SHA-256 已知答案：长输入（跨块边界）。
    #[test]
    fn sha256_long_input() {
        let mut hasher = Sha256::new();
        hasher.update(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq");
        let hash = hasher.finalize();
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex,
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }
}
