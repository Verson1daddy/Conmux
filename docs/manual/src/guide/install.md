# 装上它

这一页帮你把 Conmux 装到自己的 Windows 上。先说清楚：现在**还没有现成的安装包可下**，所以要么用 `cargo` 装命令行版，要么从源码自己构建图形界面版。听起来有点手动，但每一步都写全了，跟着走就行。

## 先看看你的机器够不够

- **系统**：Windows 10（1809 及以上版本）或 Windows 11。Conmux 只跑 Windows——它整个存在的理由就是走 Windows 原生这条路，所以没有 macOS / Linux 版。
- **想装命令行版（crate）**：需要 [Rust](https://www.rust-lang.org/tools/install) 工具链，且带 **MSVC** 组件（装 Rust 时选默认的 `x86_64-pc-windows-msvc` 就对了）。
- **想从源码构建图形界面版**：在上面 Rust 的基础上，再加 [Node.js](https://nodejs.org/) **18 或更高**（自带 `npm`），以及 [Git](https://git-scm.com/)。

> **小提示**：如果你只想先尝个鲜、看看 Conmux 能干嘛，先装下面第 ① 条的命令行版最省事——不用装 Node，几分钟搞定。图形界面留到你确定要用再构建。

## 三条路，挑一条走

### ① 命令行版（`conmux` crate）——最轻、最快

一条命令的事：

```powershell
cargo install --locked conmux
```

装完你就有了 `conmux` 命令，可以直接开会话、分屏、detach / attach（怎么用见 [你的第一个会话与分屏](first-session.md)）。这一档**不带图形界面**，是内存占用最小的那档，也是 [「这是什么」](what-and-why.md) 里说的「追求极致省内存就选它」的那个。

如果你是想**在自己的 Rust 项目里**把 Conmux 当库用（而不是当命令行工具跑），那就不用 `cargo install`，改在你项目的 `Cargo.toml` 里加一行：

```toml
[dependencies]
conmux = "0.1"
```

### ② 从源码构建图形界面版（conmux-app）

图形界面版目前只能自己构建。四步：

```powershell
git clone https://github.com/Verson1daddy/Conmux.git
cd Conmux\conmux-app
npm install
npm run tauri:build
```

构建成功后，**Windows 安装包（NSIS 格式）**在这个目录下：

```
conmux-app\src-tauri\target\release\bundle\nsis\
```

里面会有一个 `.exe` 安装程序，双击它就能把 Conmux 装进系统。

> **可能踩的坑**：
> - `npm run tauri:build` 第一次跑会比较久——它要编译整个 Rust 后端，头一回几分钟到十几分钟都正常，别以为卡死了。
> - 如果 Rust 或 Node 没装好，这一步会在中途报错；回到上面「先看看你的机器够不够」补齐再重来。
> - 安装包是**未签名**的（原因见下），双击安装时 Windows 会弹 SmartScreen 提示——处理办法同样见下。

### ③ 以后从 Releases 直接下安装包 · 🚧 路线图（还没做）

将来会在 [GitHub Releases](https://github.com/Verson1daddy/Conmux/releases) 页放**预编译好的安装包**，到时候就不用自己构建了。**现在还没有**，所以图形界面版目前只能走上面第 ② 条从源码构建。这条路等安装包发布后再回来看。

## 关于「未签名」和那个吓人的蓝框

不管是自己构建的安装包，还是以后 Releases 上的安装包，都会是**未签名**的——因为这是一个学生的开源项目，没有预算买代码签名证书。

后果是：双击安装时，Windows **SmartScreen** 会弹一个蓝色框，说「Windows 已保护你的电脑」「发布者：未知」。这**不代表软件有问题**，只是 Windows 对没花钱买证书的程序一律这么提示。想继续装，点：

**更多信息 → 仍要运行**

框就过去了，正常安装。如果你实在不放心这个提示，那就走第 ① 条命令行版——`cargo install` 走的是 Rust 官方的包分发，完全不涉及 SmartScreen 这一套。

## 装好了，然后呢？

- 想立刻上手 → [你的第一个会话与分屏](first-session.md)
- 想先搞清楚它到底解决什么问题 → [这是什么、为什么值得用](what-and-why.md)
