# Troubleshooting

Won't install, clicks do nothing, the command line spits out a wall of errors you can't parse — this page lists the pitfalls people hit most often with Conmux, laid out as "Symptom → Why → What to do". Just jump to the one you're hitting.

> If your problem isn't here, or the fix doesn't work, [open an issue and tell me](https://github.com/Verson1daddy/Conmux/issues) — include your Windows version and the exact error text; that makes it much easier to track down.

---

## "Windows protected your PC" when installing the GUI (SmartScreen · unknown publisher)

**Symptom**: You double-click the Conmux GUI installer and Windows pops up a blue dialog saying "Windows protected your PC" and "Unknown publisher", with only a "Don't run" button.

**Why**: This is the standard treatment for an **unsigned program** — not a virus, not a broken install. Digitally signing a program means buying a code-signing certificate, and Conmux is a student's open-source project with **no budget for one** — so SmartScreen doesn't recognize the publisher and blocks it first. Details in [the FAQ's unsigned-build entry](faq.md).

**What to do**:

1. Click **"More info"** in the blue dialog.
2. A **"Run anyway"** button appears at the bottom — click it.

You only have to do this once; after that everything behaves normally. If the dialog still makes you uneasy, you can build from source yourself — see [Install It](../guide/install.md) — the `conmux` command-line tool installs via `cargo install` and never touches this flow at all.

---

## GUI won't open / white screen (missing WebView2)

**Symptom**: The install finished, but Conmux's graphical interface won't start, or opens to a blank white screen.

**Why**: The Conmux GUI shell relies on the system's built-in **WebView2** to render its interface (which is also why its memory floor sits at a few hundred MB and it isn't "ultra-lightweight" — see [What It Is & Why It's Worth Using](../guide/what-and-why.md)).

- **Windows 11**: WebView2 ships with the system; usually nothing to worry about.
- **Windows 10**: **Some older machines don't have it preinstalled**, which gets you the white screen or a failure to start.

**What to do**: On Win10, download the **"Evergreen WebView2 Runtime"** from Microsoft's official site, install it, then relaunch Conmux. Once it's installed the GUI picks it up on its own — no extra configuration.

> The command-line-only `conmux` tool doesn't need WebView2 — if you only use the CLI, this one doesn't apply to you.

---

## Pressing `Ctrl+B` does nothing (the leader key is a two-step gesture)

**Symptom**: You want to split panes or switch panes, you press `Ctrl+B`, and nothing happens on screen.

**Why**: This is **most likely normal**. Conmux keeps tmux's **two-step leader** gesture: `Ctrl+B` is just the knock on the door — it does nothing by itself. You have to **release it first, then press a second key** to trigger an action.

```
Ctrl+B  then press  \        # split vertically
Ctrl+B  then press  -        # split horizontally
Ctrl+B  then press  ← ↑ ↓ →  # move focus between panes
Ctrl+B  then press  z        # zoom the current pane to full screen (press again to restore)
```

After you press `Ctrl+B`, the status bar lights up a **⌨ LEADER** badge that means "waiting for your second key" — if you see it, the leader was received.

**If even the badge doesn't light up**:

- Make sure focus is in the Conmux window (and some other program isn't grabbing the key).
- **Historical pitfall**: the leader key used to default to `Ctrl+Space`, but on **Chinese Windows** the IME's global Chinese/English toggle hotkey swallows it before Conmux ever sees it — so since v0.1 the **default has been changed to `Ctrl+B`**. If you're on a very old build and `Ctrl+Space` does nothing, just upgrade to a newer version.
- Want a different leader key? Reconfigure it via "Set leader prefix" in the command palette (the leader must include `Ctrl` or `Alt`; bare keys are rejected, so your everyday typing doesn't get swallowed as a leader).

> Two steps feel like a chore? There's an **off-by-default** leader-free direct-shortcut mode (`Ctrl+Alt+\` / `-` / `Z`, plus vim-style `Ctrl+Alt+H/J/K/L` to switch panes). It ships off so it never steals a single keystroke until you turn it on yourself.

---

## Tools from WSL won't start

**Symptom**: You try to hook up a command from WSL as a pane, but the pane exits as soon as it opens, or you get errors like "wsl not found" or "no such distribution".

**Why**: Today's Conmux **directly invokes the `wsl.exe` on your system** to start that pane — it doesn't bundle WSL, and it hasn't built "unified supervision across WSL" yet (that's on the **M4 roadmap**, see [Differences at a Glance](../from-tmux/differences.md)). So failures like these are almost always a problem with `wsl` itself, unrelated to Conmux's split panes or supervision:

- **WSL isn't installed at all** — run `wsl -l -v` on its own in a terminal; if that errors too, you need to install WSL first (Microsoft's official `wsl --install`).
- **Distribution name mismatch** — the distribution name you specified (say `Ubuntu-22.04`) doesn't match what's actually installed. `wsl -l -v` lists what you really have; fill in the name exactly as listed.

**What to do**: First get that `wsl` command running in a **plain terminal** (install WSL if it needs installing, fix the distribution name if it needs fixing). Once it runs on its own, bring the same command into Conmux and it'll just work.

---

## `cargo build` fails with schannel / CRL / certificate revocation errors

**Symptom**: When building from source, or running `cargo install --locked conmux`, `cargo` gets stuck fetching dependencies and reports errors mentioning `schannel`, `CRL`, or `certificate revocation`.

**Why**: This is **an old Windows networking ailment, not a Conmux problem**. Windows's schannel is strict about certificate revocation list (CRL) checks, and on **corporate networks / behind a proxy** it often can't fetch the CRL — so the whole TLS handshake fails.

**What to do**: Tell `cargo` to skip the revocation check — set an environment variable and retry:

```powershell
$env:CARGO_HTTP_CHECK_REVOKE = "false"
cargo install --locked conmux
```

To make it permanent, add `CARGO_HTTP_CHECK_REVOKE=false` to your system environment variables. Switching to a Chinese mirror sometimes dodges it too, but that only masks the symptom — the switch above is aimed at the root cause.

---

Still stuck after reading all this? Send the **exact error text + your Windows version (Win10/Win11) + whether it's the GUI or the command line** to an [issue](https://github.com/Verson1daddy/Conmux/issues), and I'll follow up as soon as I can.