# 排障

装不上、点了没反应、命令行报一串看不懂的错——这一页把 Conmux 上最常撞见的几个坑，按「症状 → 为什么 → 怎么办」列出来。挑你遇到的那条看就行。

> 如果这里没有你的问题，或者照着做还是不行，[开个 issue 告诉我](https://github.com/Verson1daddy/Conmux/issues)——附上你的 Windows 版本和报错原文，最好排查。

---

## 装 GUI 时弹「Windows 已保护你的电脑」（SmartScreen · 发布者未知）

**症状**：双击 Conmux 的 GUI 安装包，Windows 弹一个蓝框，写着「Windows 已保护你的电脑」「未知发布者」，只有一个「不运行」按钮。

**为什么**：这是**未签名程序**的正常待遇，不是病毒也不是装坏了。给程序做数字签名要买一张代码签名证书，而 Conmux 是一个学生的开源项目，**没有这笔预算**——所以 SmartScreen 认不出发布者，就先拦一下。详情见 [关于 & 边界](../about.md)。

**怎么办**：

1. 在蓝框里点 **「更多信息」**（More info）。
2. 底下会多出一个 **「仍要运行」**（Run anyway）按钮，点它。

只用点这一次，之后就正常了。实在不放心这个弹框，也可以[从源码自己构建](../guide/install.md)——`conmux` 命令行工具走 `cargo install` 安装，完全不碰这个流程。

---

## GUI 打不开 / 白屏（缺 WebView2）

**症状**：装完了，但 Conmux 的图形界面起不来，或者开出来是一片白。

**为什么**：Conmux 的 GUI 壳靠系统自带的 **WebView2** 来渲染界面（这也是它内存地板在几百 MB、不是「极致轻量」的原因，见 [这是什么](../guide/what-and-why.md)）。

- **Windows 11**：WebView2 是系统内置的，一般不用管。
- **Windows 10**：**部分老机器没预装**，就会白屏或起不来。

**怎么办**：如果是 Win10，去微软官网下 **「Evergreen WebView2 Runtime」** 装上，再重开 Conmux。装好后 GUI 会自己接管，不用额外配置。

> 纯命令行的 `conmux` 工具不吃 WebView2——如果你只用 CLI，这条与你无关。

---

## 按了 `Ctrl+B` 没反应（前缀键是两步手势）

**症状**：想分屏或切 pane，按了 `Ctrl+B`，屏幕上什么都没发生。

**为什么**：这**多半是正常的**。Conmux 沿用 tmux 的**两步前缀**手势：`Ctrl+B` 只是「叫一下」,它本身不干活，得**先松开、再按第二个键**才触发动作。

```
Ctrl+B  然后按  \        # 竖着分屏
Ctrl+B  然后按  -        # 横着分屏
Ctrl+B  然后按  ← ↑ ↓ →  # 在 pane 之间切焦点
Ctrl+B  然后按  z        # 当前 pane 放大到全屏（再按一次还原）
```

按下 `Ctrl+B` 后，状态栏会亮一个 **⌨ LEADER** 徽章提示「我在等你的第二个键」——看到它就说明前缀收到了。

**如果连徽章都不亮**：

- 确认焦点在 Conmux 窗口里（不是别的程序抢了键）。
- **历史坑**：前缀键默认曾是 `Ctrl+Space`，但在**中文 Windows** 上会被输入法的「中/英切换」全局热键吃掉，Conmux 根本收不到——所以从 v0.1 起**默认已改成 `Ctrl+B`**。如果你用的是很旧的构建、按 `Ctrl+Space` 没反应，升级到新版即可。
- 想换成别的前缀键，可以在命令面板里「设置 leader 前缀」重配（前缀必须带 `Ctrl` 或 `Alt`，裸键会被拒绝，免得每次打字都被吞成前缀）。

> 嫌两步麻烦？有一个**默认关闭**的免前缀直接快捷键模式（`Ctrl+Alt+\` / `-` / `Z`，以及 vim 风格的 `Ctrl+Alt+H/J/K/L` 切 pane）。默认不开，是为了在你亲手打开前不抢走任何按键。

---

## WSL 里的工具起不来

**症状**：想把一个 WSL 里的命令接成 pane，结果 pane 一开就退，或者报「找不到 wsl」「没有该分发版」之类的错。

**为什么**：今天的 Conmux 是**直接调用你系统上的 `wsl.exe`** 来起这个 pane 的——它没有自带 WSL，也还没做「跨 WSL 的统一监管」（那是 **M4 路线图**，见 [从 tmux 过来](../from-tmux/differences.md)）。所以这类失败基本都是 `wsl` 本身的问题，跟 Conmux 的分屏/监管无关：

- **WSL 根本没装**——命令行里单独敲 `wsl -l -v`，如果它自己也报错，说明得先装 WSL（微软官方的 `wsl --install`）。
- **发行版名字对不上**——你指定的分发版名（比如 `Ubuntu-22.04`）和实际安装的对不上。`wsl -l -v` 会列出你真正装了哪些，照着列出来的名字填。

**怎么办**：先在一个**普通终端**里把这条 `wsl` 命令跑通（该装 WSL 装 WSL、该对齐分发版名对齐），能单独跑起来后，再把同样的命令接进 Conmux 就顺了。

---

## `cargo build` 报 schannel / CRL / 证书吊销错误

**症状**：从源码构建、或 `cargo install --locked conmux` 时，`cargo` 卡在拉依赖，报 `schannel`、`CRL`、或 `certificate revocation` 相关的错。

**为什么**：这是 **Windows 网络层的老毛病，不是 Conmux 的问题**。Windows 的 schannel 对证书吊销列表（CRL）校验很严，在**企业网 / 走代理**的环境下常常拉不到 CRL，于是整个 TLS 握手就失败了。

**怎么办**：让 `cargo` 别做这一步吊销校验——设一个环境变量再重试：

```powershell
$env:CARGO_HTTP_CHECK_REVOKE = "false"
cargo install --locked conmux
```

想永久生效，就把 `CARGO_HTTP_CHECK_REVOKE=false` 加进系统环境变量。换用国内镜像有时也能绕过，但那只是掩盖症状——上面这个开关才是对着根因。

---

看完还没解决？把**报错原文 + 你的 Windows 版本（Win10/Win11）+ 是 GUI 还是命令行**一起发到 [issue](https://github.com/Verson1daddy/Conmux/issues)，我尽快跟。
