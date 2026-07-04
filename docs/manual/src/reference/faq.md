# 常见问题（FAQ）

这里收了大家最常问的几个问题，一问一答，说人话。凡是「还没做」的地方都标出来，绝不含糊。

## 我得先装 WSL 吗？

**不用。** Conmux 直接跑在 Windows 自己的终端底层（**ConPTY**）上，每个面板都是一个真正的 Windows 伪终端。装上就能跑 PowerShell、`cmd`、任何原生命令行程序，不需要在底下垫一层 Linux 子系统。

那 WSL 里的工具怎么办？**能接**——你在一个面板里直接跑 `wsl.exe <你的工具>`，它就跟普通命令一样被 Conmux 接管、监管、断线重连。这条路今天就能用。

> **老实说**：把 Windows 侧和 WSL 侧的进程**统一在一棵会话树里**监管（比如跨边界优雅地关掉 WSL 进程、路径自动翻译）——这一层还没做，是路线图上的 **M4**。今天你能在面板里跑 WSL 工具，但那种「一视同仁的统一监管」还在路上。

## 它和 tmux 是什么关系？

Conmux 是**类 tmux** 的东西——一个终端复用器（把多个会话装进一个窗口、分屏、断线重连）。但它**不是 tmux 的移植版，也不是 drop-in 替换**：

- **tmux 不为 Windows 而生**，官方上游也明确不会去做原生 Windows；在 Windows 上用 tmux，你得先钻进 WSL。
- **Conmux 从头就是 Windows 原生的**——跑在 ConPTY 上，不走 Unix 模拟层。

所以 tmux 的配置文件、`.tmux.conf`、那套快捷键，**照搬不过来**。Conmux 有自己的一套（GUI 里默认前缀键是 `Ctrl+B`——特意和 tmux 一致，方便老手，但底下是两套实现）。如果你是 tmux 老手，看 [从 tmux 过来 · 差异速查表](../from-tmux/differences.md) 最省事。

## 它很占内存吗？

**看你用哪一层，答案不一样，这里给实话：**

- **带图形界面的 Conmux（conmux-app）**：用的是 Tauri + 系统自带的 **WebView2**。这**不是**最极致的轻量方案，内存地板大约**几百 MB**。它比「把 agent 塞进一整套 IDE（VS Code 加一堆插件）」省得多，但要说「极致省内存」，它不是。
- **纯命令行的 `conmux`（不带 GUI 的那个 crate / CLI）**：**很轻**。它是纯 Rust 的机制层内核，依赖树刻意压得很小，没有编辑器、没有浏览器引擎那一大坨。

一句话：追求轻量、只在命令行里活动 → 用纯 CLI 的 `conmux`；想要可视化分屏、会话点阵、深度观测面板 → 用 GUI 壳，接受几百 MB 的地板。

## 支持 macOS / Linux 吗？

**不支持，这是设计使然，不是没来得及做。** Conmux 是 **Windows only**：它的整个立身之本就是「把 tmux 那类能力做到 Windows 原生」——ConPTY 伪终端、Job Object 整树监管、命名管道 IPC，全是 Windows 平台的东西。在别的系统上，你本来就有 tmux / Zellij。

技术上：GUI 壳只在 Windows（Win10 1809+ / Win11）编译运行。`conmux` crate 的**纯逻辑层**跨平台可编译测试（方便开发），但真正干活的 ConPTY / Job Object 后端只在 Windows 上编。

## 它和 Conflux 是什么关系？

简单说：**Conmux 是地基，Conflux 是盖在上面的楼。**

- **Conmux** 只懂三样东西——面板、进程、字节。它**不知道什么叫「agent」**，也不想知道。这条「只做机制、不碰语义」的边界，正是它能当地基的原因。它是独立产品：单独下载 Conmux，你拿到的就是一个终端复用器，仅此而已。
- **Conflux** 建在 Conmux 之上，是一个**多 agent 的 CLI 监管台（GUI）**——把「agent」这层语义加回来：谁在等你、谁弹了权限、多个 agent 怎么摆布。

所以：想要一个 Windows 原生的终端复用地基 → Conmux；想要在它之上做多 agent 的可视化监管 → Conflux。

## 没签名，装它安全吗？

先说清楚现状：GUI 安装包会是**未签名**的——学生开源项目，没预算买代码签名证书。Windows SmartScreen 会提示「发布者未知」。可以点「更多信息 → 仍要运行」，或者干脆自己从源码构建。（纯 CLI 的 `conmux` 走 `cargo install --locked conmux` 安装，完全不涉及这个提示。）

**为什么可以放心：**

- **源码全开**——MIT / Apache-2.0 双许可，代码你都能看、能审、能自己 `cargo build` / `npm run tauri:build` 出一份你亲手编的版本。
- **本地无遥测**——没有账号、没有分析上报，**没有任何数据离开你的机器**。连接级审计（谁连了 daemon）只写到本地一个滚动日志 `%LOCALAPPDATA%\conmux\daemon.log`，不外发。
- **信任边界是「当前用户」**——daemon 走命名管道，DACL 只放行你自己的账号（SID），并拒绝远程客户端。

> **诚实边界**：命名管道那层的身份校验（客户端 fail-closed、客户端用 Authenticode 核对 daemon 的进程镜像）是为了「抬高门槛、保持可审计」，**不是**用来防同一账号下已经在跑的恶意代码——任何以你的身份运行的程序，本来就能读你的内存、杀你的进程。这是操作系统的边界，不是 Conmux 能替你补上的。这一点我们摊开讲，不假装。

---

还有没答上的问题？欢迎[开 issue 或 discussion](https://github.com/Verson1daddy/Conmux/issues)，也帮我们把这份 FAQ 补得更全。
