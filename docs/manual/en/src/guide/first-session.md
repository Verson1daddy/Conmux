# Your First Session & Split Panes

Installed? Good — let's get it running and cover the handful of moves you'll use most, in five minutes.

*(This page is about the Conmux GUI. The command-line-only `conmux` tool goes a different route — see [Driving conmux from Code](../advanced/control-plane.md).)*

## What it looks like on launch

Open the Conmux GUI and you'll see a window running a command-line session — just like opening a terminal normally. Type whatever you want to run into it (an agent CLI, a build, a local server, your call).

The window has a **strip of session dots along the bottom**: every session you open adds one dot. **Click a dot and you jump to that session.** Once you have several sessions going, this row of dots is your at-a-glance view of "where am I, and what else is running."

> These are the session dots from the previous page — "a row of dots, click one to jump over." Sessions never interfere with each other; each runs on its own.

## The leader prefix: Conmux's "secret handshake"

Splitting panes, moving focus — Conmux triggers these actions with a **leader key**, **`Ctrl+B`** by default (tmux veterans will find this familiar).

It's a **two-step gesture**, not four keys pressed at once:

1. Press **`Ctrl+B`** once and release — this keystroke doesn't reach your program; Conmux enters a "standby" state (a small on-screen hint tells you it's waiting for the next key).
2. **Then press a single command key** — say `\`, `-`, or `z` — and Conmux performs the corresponding action.

> **Why two steps?** This way Conmux **never touches your keyboard** in normal use: apart from that one `Ctrl+B` key, every other keystroke passes straight through to your program. Only the keystroke right after `Ctrl+B` gets treated as a Conmux command. This is a hard guarantee from the kernel — **it will never break your CLI**.
>
> Two thoughtful details while we're at it: after pressing `Ctrl+B`, if you don't press a command key within **1.5 seconds**, it automatically falls back to pass-through (you didn't fat-finger anything; pretend nothing happened). And if you actually want to send the `Ctrl+B` key itself to your program, press `Ctrl+B` then `Ctrl+B` again — the literal prefix gets delivered to the current session.

## Split panes: one window, several cells

Press `Ctrl+B` and release, then press:

| Then press | Effect |
|-----------|------|
| **`\`** (backslash) | **Vertical split** — the current pane splits left/right, with a new session opening on the right |
| **`-`** (minus) | **Horizontal split** — the current pane splits top/bottom, with a new session opening below |

Want a 2×2 grid? Just split a few more times — one vertical split, then one horizontal, arrange it however you like. Each cell is a "pane," each running its own session.

## Moving focus between panes

Once you've split, keyboard input needs a notion of "which cell am I in right now." Press `Ctrl+B` and release, then press an **arrow key**, and focus jumps to the pane in that direction:

- `Ctrl+B` then **`←` / `→` / `↑` / `↓`** — move focus to the pane on the left / right / above / below.

Whichever pane has focus is where your keystrokes go.

## Getting a closer look: zoom

After splitting, every cell gets smaller. Want to temporarily blow one cell up to fill the whole window?

- `Ctrl+B` then **`z`** — the currently focused pane **zooms to full screen**; do `Ctrl+B` `z` again to **restore** the original pane layout.

(`z` = zoom. The other panes aren't closed, just temporarily hidden — restore and they're back.)

## Leave and come back: detach / attach

This is the thing Conmux most wants you to feel safe about:

**Close the Conmux window outright, and the sessions inside don't die.** They keep running in the background. Open Conmux again and **the picture reconnects exactly as it was** — wherever things had gotten to, whatever was on screen, it's all there (scrollback history, cursor, even the UI state of full-screen programs gets replayed as-is).

This is **detach / attach**. It's especially useful for long-running tasks: close the window, go do something else, come back and pick up watching — nothing lost.

> **Honest note**: "close the window and sessions don't die" refers to closing the **client window**. The sessions' real host is the background daemon; if you explicitly stop the daemon (`conmux kill-server`) or it crashes on its own, that's when all sessions end together — this is the other side of "never leave orphan processes," a deliberate design choice, not a bug.

## Not there yet (roadmap)

Marking the boundaries honestly, so you don't press something, get no response, and assume it's broken:

- **The GUI itself is still under construction** — the split, focus, and zoom keybindings above have frozen specs and interactions, and the code has landed, but the full native GUI shell is the current mainline development effort, and details may still be getting polished.
- **Persistence across daemon restarts** — right now detach/attach means reconnecting while the daemon stays alive; "sessions survive a reboot" style cross-process persistence is **out of scope for now** (an explicit design trade-off).
- **Remote attach / unified cross-WSL support** — both still on the roadmap; see [Differences at a Glance](../from-tmux/differences.md) and the Roadmap in the conmux repo.

## What's next

- tmux veteran wondering what's different? → [Differences at a Glance](../from-tmux/differences.md)
- Want to drive it from code for automation? → [Driving conmux from Code](../advanced/control-plane.md)
