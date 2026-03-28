# Limux Shortcut Remapping

This document explains how the Linux host shortcut system works and how to test it manually.

## What It Does

Limux has a host-owned shortcut registry in `rust/limux-host-linux/src/shortcut_config.rs`.

That registry is the single source of truth for:

- default shortcut bindings
- user overrides from config
- GTK app accelerators
- capture-phase host shortcut dispatch
- visible tooltip text for shortcut-backed UI actions

Ghostty config is not involved. Ghostty still owns terminal behavior once Limux decides not to intercept a key.

## Config File Location

Limux reads shortcuts from:

```text
~/.config/limux/config.json
```

That path comes from `dirs::config_dir()/limux/config.json`.

If the file is missing, Limux uses built-in defaults.

## Important Runtime Behavior

- Shortcuts are loaded at startup.
- When you change them through the terminal `Keybinds` editor, Limux writes the config, reloads it, and applies the new bindings immediately in the running app.
- If you edit `~/.config/limux/config.json` by hand outside the app, restart Limux to pick up those changes.
- If the config file is invalid or unreadable, Limux falls back to defaults and prints a warning to stderr.
- If two active shortcuts resolve to the same binding, Limux rejects the override set and falls back to defaults.
- Unknown shortcut IDs are ignored with a warning.
- `null` or `""` unbinds a shortcut.
- Host shortcuts must use `Ctrl` or `Alt` as the base modifier. `Shift` can be added on top of that.

## Keybinds Editor

The terminal right-click menu now includes `Keybinds`.

Selecting it opens a popover editor that:

- lists every host-owned shortcut
- shows the current binding
- shows the default binding
- lets you click a binding pill to enter listening mode
- closes from the top-right `×` button
- also closes when you click outside the popover

Capture rules:

- valid examples:
  - `Ctrl+H`
  - `Ctrl+Shift+H`
  - `Alt+X`
- rejected examples:
  - plain `H`
  - `Shift+H`
  - modifier-only keys like `Ctrl`

If a capture is invalid or duplicates another active shortcut, the row shows an inline error and keeps the previous working binding.

## Config Format

Top-level shape:

```json
{
  "shortcuts": {
    "toggle_sidebar": "<Ctrl><Alt>b",
    "split_right": null,
    "new_terminal": ""
  }
}
```

Rules:

- Keys must be under `"shortcuts"`.
- Values must be either:
  - a GTK-style accelerator string like `"<Ctrl><Shift>n"`
  - `null` to unbind
  - `""` to unbind
- Omitted keys keep their defaults.

## Supported Shortcut IDs

These are the current supported config keys and defaults:

| Config key | Default |
|---|---|
| `new_workspace` | `<Ctrl><Shift>n` |
| `close_workspace` | `<Ctrl><Shift>w` |
| `toggle_sidebar` | `<Ctrl>b` |
| `next_workspace` | `<Ctrl>Page_Down` |
| `prev_workspace` | `<Ctrl>Page_Up` |
| `cycle_tab_prev` | `<Ctrl><Shift>Left` |
| `cycle_tab_next` | `<Ctrl><Shift>Right` |
| `split_down` | `<Ctrl><Shift>d` |
| `new_terminal_in_focused_pane` | `<Ctrl><Shift>t` |
| `split_right` | `<Ctrl>d` |
| `close_focused_pane` | `<Ctrl>w` |
| `new_terminal` | `<Ctrl>t` |
| `focus_left` | `<Ctrl>Left` |
| `focus_right` | `<Ctrl>Right` |
| `focus_up` | `<Ctrl>Up` |
| `focus_down` | `<Ctrl>Down` |
| `activate_workspace_1` | `<Ctrl>1` |
| `activate_workspace_2` | `<Ctrl>2` |
| `activate_workspace_3` | `<Ctrl>3` |
| `activate_workspace_4` | `<Ctrl>4` |
| `activate_workspace_5` | `<Ctrl>5` |
| `activate_workspace_6` | `<Ctrl>6` |
| `activate_workspace_7` | `<Ctrl>7` |
| `activate_workspace_8` | `<Ctrl>8` |
| `activate_last_workspace` | `<Ctrl>9` |

## Dispatch Model

There are two host shortcut paths, both driven by the same resolved registry:

1. GTK accelerators
   - Used for:
     - `new_workspace`
     - `close_workspace`
     - `toggle_sidebar`
     - `next_workspace`
     - `prev_workspace`
2. Capture-phase key dispatch
   - Used for everything in the table above, including the GTK-backed actions

That means a remap changes both the GTK accelerator registration and the capture-phase match.

## Pass-Through Behavior

If a key combo does not match a resolved Limux shortcut, Limux does not intercept it and Ghostty receives it.

That means terminal-native combos like these should pass through unless you explicitly bind them in Limux:

- `Ctrl+C`
- `Ctrl+L`
- `Ctrl+R`
- plain typing
- `Enter`

This is the behavior you want when testing that unbound shortcuts stop being stolen by the host.

## Visible Tooltip Behavior

These UI surfaces currently reflect shortcut overrides:

- sidebar collapse button
- sidebar expand button
- pane header buttons for:
  - new terminal tab
  - split right
  - split down
  - close pane

These surfaces do not currently show a shortcut suffix:

- new browser tab button
- browser navigation buttons (`Back`, `Forward`, `Reload`)

Note:

- `new_terminal` and `new_terminal_in_focused_pane` both dispatch to the same terminal-tab creation command today.
- The pane header tooltip uses `new_terminal`, not `new_terminal_in_focused_pane`.

## Launch Commands

From the repo root:

```bash
cargo test -p limux-host-linux
cargo build -p limux-host-linux --features webkit
```

Run the app for manual testing:

```bash
LD_LIBRARY_PATH="/home/willr/Applications/cmux-linux/cmux/ghostty/zig-out/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
cargo run -p limux-host-linux --features webkit --bin limux
```

## Manual Test Plan

### 1. Baseline Defaults

Remove or move the config file out of the way:

```bash
trash ~/.config/limux/config.json
```

Launch Limux and verify:

- `Ctrl+B` toggles the sidebar
- `Ctrl+T` opens a terminal tab
- `Ctrl+D` splits right
- `Ctrl+Shift+D` splits down
- `Ctrl+W` closes the focused pane
- `Ctrl+Page_Down` and `Ctrl+Page_Up` switch workspaces
- sidebar tooltips show `Ctrl+B`
- pane button tooltips show the default shortcut suffixes where applicable

### 2. Remap One Shortcut

Create:

```json
{
  "shortcuts": {
    "toggle_sidebar": "<Ctrl><Alt>b"
  }
}
```

Restart Limux and verify:

- `Ctrl+Alt+B` toggles the sidebar
- `Ctrl+B` no longer toggles the sidebar
- sidebar tooltips show `Ctrl+Alt+B`

### 3. Unbind One Shortcut

Create:

```json
{
  "shortcuts": {
    "split_right": null
  }
}
```

Restart Limux and verify:

- `Ctrl+D` no longer triggers split-right in Limux
- the split-right button tooltip no longer shows a shortcut suffix
- in a terminal pane, `Ctrl+D` now reaches the terminal app instead of being intercepted by Limux

### 4. Verify Pane Tooltip Remap

Create:

```json
{
  "shortcuts": {
    "new_terminal": "<Ctrl><Alt>t",
    "close_focused_pane": "<Ctrl><Alt>w"
  }
}
```

Restart Limux and verify:

- pane button tooltips show `Ctrl+Alt+T` and `Ctrl+Alt+W`
- `Ctrl+Alt+T` opens a terminal tab
- `Ctrl+T` no longer opens a terminal tab
- `Ctrl+Alt+W` closes the focused pane
- `Ctrl+W` no longer closes the pane

### 5. Duplicate-Binding Rejection

Create:

```json
{
  "shortcuts": {
    "toggle_sidebar": "<Ctrl><Alt>b",
    "split_right": "<Ctrl><Alt>b"
  }
}
```

Restart Limux from a terminal and verify:

- Limux prints a warning about duplicate bindings
- Limux falls back to defaults
- `Ctrl+B` toggles the sidebar
- `Ctrl+D` still splits right

### 6. Open The Keybinds Editor

Launch Limux, right-click inside a terminal, and verify:

- the terminal context menu contains `Keybinds`
- clicking `Keybinds` opens the keybind editor popover
- the editor shows a row for every host-owned shortcut
- each row shows both the current binding and the default binding
- clicking the `×` button closes the popover
- clicking outside the popover also closes it

### 7. Remap From The Editor

Launch Limux, open terminal `Keybinds`, click the `Split Right` binding, and press `Ctrl+H`.

Verify:

- the `Split Right` row updates to `Ctrl+H`
- `~/.config/limux/config.json` contains the `split_right` override
- `Ctrl+H` splits right immediately without restarting Limux
- `Ctrl+D` no longer splits right
- the pane header split-right tooltip now shows `Ctrl+H`

### 8. Editor Validation

Launch Limux, open terminal `Keybinds`, and try these invalid captures on any row:

- press only `Shift+H`
- press only `Ctrl`
- assign a combo already used by another shortcut

Verify:

- the row shows an inline error
- the previous binding remains visible after the error
- the running app keeps the old working shortcut

### 9. Unknown ID Handling

Create:

```json
{
  "shortcuts": {
    "toggle_sidebar": "<Ctrl><Alt>b",
    "not_a_real_shortcut": "<Ctrl>x"
  }
}
```

Restart and verify:

- Limux warns that the unknown ID was ignored
- `toggle_sidebar` still remaps correctly

### 10. Invalid JSON Fallback

Write invalid JSON:

```json
{ this is not valid json
```

Restart Limux from a terminal and verify:

- Limux prints a warning
- Limux falls back to defaults
- default shortcuts work again

## Good Test Cases

If you only want a short smoke test, do these three:

1. Remap `toggle_sidebar` to `<Ctrl><Alt>b`
2. Unbind `split_right`
3. Remap `new_terminal` to `<Ctrl><Alt>t`

That covers:

- GTK accelerators
- capture-phase dispatch
- visible tooltips
- old-binding disablement
- pass-through after unbind

## Relevant Source Files

- `rust/limux-host-linux/src/shortcut_config.rs`
- `rust/limux-host-linux/src/main.rs`
- `rust/limux-host-linux/src/window.rs`
- `rust/limux-host-linux/src/pane.rs`
