# Install It

This page walks you through getting Conmux onto your own Windows machine. Up front: there is **no prebuilt installer to download yet**, so you either install the CLI version with `cargo` or build the GUI version from source. Sounds a bit manual, but every step is written out — just follow along.

## Check Your Machine First

- **OS**: Windows 10 (version 1809 or later) or Windows 11. Conmux runs on Windows only — its entire reason for existing is going the Windows-native route, so there is no macOS / Linux build.
- **For the CLI version (crate)**: you need the [Rust](https://www.rust-lang.org/tools/install) toolchain, with the **MSVC** component (picking the default `x86_64-pc-windows-msvc` when installing Rust is exactly right).
- **For building the GUI version from source**: on top of Rust above, add [Node.js](https://nodejs.org/) **18 or later** (which ships with `npm`), plus [Git](https://git-scm.com/).

> **Tip**: if you just want a quick taste of what Conmux can do, installing the CLI version under option ① below is the least hassle — no Node required, done in minutes. Save the GUI build for when you're sure you'll use it.

## Three Paths — Pick One

### ① The CLI version (the `conmux` crate) — lightest, fastest

It's one command:

```powershell
cargo install --locked conmux
```

Once it's installed you have the `conmux` command and can start sessions, split panes, and detach / attach right away (see [Your First Session & Split Panes](first-session.md) for how). This tier **has no GUI** and is the smallest-memory-footprint one — the same one [What It Is & Why It's Worth Using](what-and-why.md) means by "if you're chasing minimal memory, pick this."

If you want to use Conmux **as a library inside your own Rust project** (rather than running it as a CLI tool), skip `cargo install` and add one line to your project's `Cargo.toml` instead:

```toml
[dependencies]
conmux = "0.1"
```

### ② Build the GUI version from source (conmux-app)

For now, the GUI version can only be built yourself. Four steps:

```powershell
git clone https://github.com/Verson1daddy/Conmux.git
cd Conmux\conmux-app
npm install
npm run tauri:build
```

After a successful build, the **Windows installer (NSIS format)** is in this directory:

```
conmux-app\src-tauri\target\release\bundle\nsis\
```

Inside you'll find a `.exe` installer — double-click it to install Conmux onto your system.

> **Pitfalls you might hit**:
> - The first run of `npm run tauri:build` takes a while — it has to compile the entire Rust backend, and anywhere from a few minutes to over ten is normal the first time. Don't assume it's hung.
> - If Rust or Node isn't set up properly, this step errors out partway through; go back to "Check Your Machine First" above, fill the gaps, and rerun.
> - The installer is **unsigned** (see below for why), so Windows pops a SmartScreen prompt when you double-click it — how to handle that is also covered below.

### ③ Download an installer straight from Releases · 🚧 Roadmap (not built yet)

Eventually there will be **prebuilt installers** on the [GitHub Releases](https://github.com/Verson1daddy/Conmux/releases) page, and you won't have to build anything yourself. **They're not there yet**, so for now the GUI version is source-build-only via option ② above. Come back to this path once installers ship.

## About "Unsigned" and That Scary Blue Box

Whether it's an installer you built yourself or one from Releases later, it will be **unsigned** — this is a student's open-source project, and there's no budget for a code-signing certificate.

The consequence: when you double-click the installer, Windows **SmartScreen** pops a blue box saying "Windows protected your PC" and "Publisher: Unknown." This **does not mean something is wrong with the software** — it's just how Windows treats every program that didn't pay for a certificate. To keep installing, click:

**More info → Run anyway**

The box goes away and installation proceeds normally. If that prompt genuinely makes you uneasy, take option ① the CLI route instead — `cargo install` goes through Rust's official package distribution and never touches the whole SmartScreen business.

## Installed — Now What?

- Want to dive in right away → [Your First Session & Split Panes](first-session.md)
- Want to first understand what problem it actually solves → [What It Is & Why It's Worth Using](what-and-why.md)