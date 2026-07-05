# Differences at a Glance

*(This page is written for people who already live in tmux. It assumes you know what a prefix, a pane, and a split are, and just want to know what's the same and what's different on Windows, in Conmux.)*

## The one-sentence version first

Conmux borrows tmux's **feel** — a leader key, then one command key to split panes, move focus, zoom — but it is **not a drop-in replacement for tmux**. It doesn't read your `.tmux.conf`, its command set is a small subset of tmux's, and it runs on a Windows-native foundation. So: your muscle memory mostly carries over, but don't expect to port your tmux config and have it just work.

## Windows-native, no WSL required

This is Conmux's number-one reason to exist. tmux was never designed for Windows — to use it there, you first install a layer of WSL (the Linux subsystem), and then your tmux actually lives inside that Linux and can't manage processes on the Windows side.

Conmux runs directly on Windows' own pseudo-terminal (**ConPTY**), **no WSL needed**. PowerShell, native programs, AI agent CLIs — they're all panes supervised under one process tree. (Want to bring in tools from WSL too? You can — but that belongs to "cross-WSL unification" (M4) on the roadmap, **which isn't built yet**; there's a [separate section](wsl.md) on what's possible today.)

## The leader key: `Ctrl+B` by default, and why not `Ctrl+Space`

Good news: Conmux's default leader is **`Ctrl+B`**, exactly the same as tmux's own default — zero onboarding cost for veterans.

We actually tried `Ctrl+Space` first (a tmux custom prefix a lot of people love). We hit a landmine: **on Chinese Windows, `Ctrl+Space` is the IME's global "Chinese/English toggle" hotkey — the IME swallows it first and Conmux never receives it**, so the leader is effectively dead in a Chinese environment. Hence the default went back to `Ctrl+B` to steer clear of it.

The leader key is configurable (in-app, persisted locally). The one hard constraint: **the leader must include `Ctrl` or `Alt`** — a bare key as leader would swallow every ordinary keystroke as a command and outright break your CLI, so that's not allowed.

> **Same small detail as tmux**: pressing the leader twice (`Ctrl+B` then `Ctrl+B`) sends a literal prefix character into the current terminal — the equivalent of tmux's `send-prefix`.

## Keybinding cheat sheet

Press the leader (default `Ctrl+B`), then the command key. Here's tmux vs. Conmux side by side:

| What you want | tmux (default) | Conmux | Notes |
|---------|-------------|--------|------|
| Vertical split (side by side) | `prefix %` | `prefix \` | Conmux uses `\`, not `%` |
| Horizontal split (stacked) | `prefix "` | `prefix -` | Conmux uses `-`, not `"` |
| Move focus between panes | `prefix ←↑↓→` | `prefix ←↑↓→` | Same |
| Resize a pane | `prefix Ctrl+←↑↓→` | `prefix Shift+←↑↓→` | Conmux uses `Shift`+arrows |
| Zoom a pane to full screen (toggle) | `prefix z` | `prefix z` | Same |
| Jump to session N | `prefix 0..9` (switches windows) | `prefix 1..9` | Conmux jumps between **sessions**, starting at 1 |
| Next / previous session | `prefix n` / `prefix p` | `prefix n` / `prefix p` | Same |
| Open the command palette | `prefix :` (command line) | `prefix :` | Conmux opens the in-app command palette |

> **Honest note**: this table covers exactly the leader commands Conmux has implemented — **there is nothing more**. tmux's session/window/pane three-level model, copy-mode, `prefix [` paging and selection, the `prefix d` detach shortcut (Conmux's detach is closing the client window, not a keybinding), command aliases, custom `bind-key` bindings — **none of these exist yet**. Don't go down a tmux cheatsheet trying them one by one.

## Leader-free direct shortcuts (opt-in, off by default)

Find the two-step leader tedious? Conmux offers a tier of **leader-free** direct shortcuts, one step and done:

- `Ctrl+Alt+H/J/K/L` → move pane focus in vim directions
- `Ctrl+Alt+\` → vertical split · `Ctrl+Alt+-` → horizontal split · `Ctrl+Alt+Z` → zoom

But it's **off by default** — you have to explicitly enable it in settings. Why off by default: Conmux's core promise is "**never break your CLI**" — in the default state it intercepts exactly **one** key, the leader, and passes everything else through to the terminal untouched. Turning on direct shortcuts means letting it intercept a small handful of `Ctrl+Alt` combos too; that trades a bit of convenience against the purity of pass-through, so the choice is yours. (Also: genuine AltGr character composition is not intercepted, and non-US keyboard layouts won't have their input broken.)

## Differences spelled out (don't step on these)

- **It doesn't read `.tmux.conf`.** Conmux has no tmux config file parsing whatsoever. The leader key and direct shortcuts are configured in-app and stored locally — completely separate from tmux's config files.
- **The command set is a subset, not the full set.** Most tmux features outside the table above don't exist; think of it as "a Windows-native multiplexer that borrows tmux's feel", not "tmux for Windows".
- **detach / attach semantics differ.** In tmux, sessions live on the server and `prefix d` detaches; in Conmux you **close the client window and the panes keep running in the background** — the next `attach` replays the screen exactly as it was (scrollback plus terminal mode state included). But note: panes outlive the **client**, not the daemon — `conmux kill-server` or a daemon crash takes every pane down with it (that's the flip side of the "zero orphan processes" guarantee; the [control plane chapter](../advanced/control-plane.md) covers this in detail).
- **It's still young.** Conmux is v0.1.x. The keys marked "Same" above are what's usable in the current implementation; the things marked "doesn't exist yet" genuinely don't. The manual flags these honestly, page by page — no selling the roadmap as the present.

## Next up

- Want to bring in tools from WSL? → [Bringing In Tools from WSL](wsl.md)
- Want to drive it from code and automate? → [Driving conmux from Code](../advanced/control-plane.md)
