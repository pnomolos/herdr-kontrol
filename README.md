# herdr-kontrol

Maschine MK3 (`17cc:1600`) or Plus (`17cc:1820`) as a glance+focus surface for [herdr](https://github.com/herdrdev/herdr) or [Rex](https://www.superlogical.com/rex).

While running, this process seizes the MK3 HID interface and claims bulk IF#5 (480×272 RGB565). It kills **NIHostIntegrationAgent** and **NIHardwareAgent** so those processes cannot steal IF#5; Maschine 3.app goes dark. SIGINT/SIGTERM join the stomp thread, blank pads, then relaunch the NI agents. `--no-restore` leaves them down (you will need `herdr-kontrol restore` or a reboot if launchd has been throttled).

HID is opened with Darwin seize. IF#5 is `USBInterfaceOpen` (not Seize) — NIHIA can still steal the pipe on respawn, which is why the stomp thread exists.

Alpha. macOS arm64. herdr: glance+focus only. Rex: also approve/deny permission prompts and a live screen tail.

## Map

| Hardware | Function |
|---|---|
| Pads 1–8 | Working set in stable workspace/pane order (status does not reshuffle pads). Press focuses the herdr pane and raises the host terminal/IDE. 9+ page via ← → (top two pad rows stay dark on page 1). |
| Groups A–H | Workspaces by `number`; press focuses |
| Dual screens | Left: selected occupant (hero/task). Right: attention queue (up to 8 rows / page) |
| Encoder | Cycle selection |
| Encoder press | `pane.focus` |
| Top buttons | Focus occupied pads 1–8 |
| ← → | Page |
| Play / Shift+Play / Stop | Rex only: answer the selected pane's permission prompt — Enter / `2` / Esc. Lit only when the selection is `blocked kind=permission`. |

## Backends

`--backend auto` (default) uses Rex if its socket answers, else herdr, and re-detects after a disconnect. Force one with `--backend rex|herdr`.

### Rex

`rex.control.v1` WebSocket over the server's unix socket (`--rex-socket`, else `REX_SERVER`, else `~/Library/Application Support/rex/server.sock`), API revision 4, `session.attach_all{event_scope:"all"}`. No polling: `session_view_changed` drives `session.view` refetches; `block_event`s (`program_status_changed/removed`, `title_changed`, `process_changed`, `child_exited`) patch blocks. Response/event `seq` ordering keeps a queued event from clobbering a newer fetch.

| Rex | Surface |
|---|---|
| Session | Group A–H |
| Block (terminal) | Pad, in session then block-id order. Blocks without OSC 7501 status are pads too, ranked last, labelled by foreground process. |
| OSC 7501 record | Most urgent record per block: `blocked` (pad hue by `kind`: permission pink, question purple, auth orange) › `error` (steady red) › `working` › `done` › `idle`. `msg` is the headline, `progress` a bar under the header. |

Agents must emit OSC 7501; Claude Code does natively.

- **Focus**: `session.focus_block`, then `client.perform session.select` on the Rex.app client so the app follows. That needs **Remote Control** enabled in Rex settings; without it, focus only moves the server's view of the session (warned once).
- **Approve/deny**: re-reads `program_status` and types via `com.superlogical.terminal.write` only if the top record is still `blocked`/`permission` with the same `updated_at` that was on screen.
- **Screen tail**: the selected block's `com.superlogical.terminal.format` text, polled at 1 Hz only while selected.

`herdr-kontrol watch [--screen]` connects to the backend without the device and prints the pad/group model — read-only, safe against a live server.

### herdr

Talks to `~/.config/herdr/herdr.sock` (`session.snapshot` + `events.subscribe` + `pane.focus` / `agent.focus`). herdr 0.9 independent client views: pad press uses `pane.focus` so the attached TUI follows; `agent.focus` alone only updates the server record.

## Install

Signed + notarized macOS arm64 binary from [Releases](https://github.com/pnomolos/herdr-kontrol/releases):

```
chmod +x herdr-kontrol
./herdr-kontrol
```

Or from source:

```
cargo build --release
./target/release/herdr-kontrol
```

Launch detached so killing the parent shell does not skip NI restore:

```
python3 -c 'import subprocess, sys; subprocess.Popen([sys.argv[1]], start_new_session=True)' ./herdr-kontrol
```

```
cargo run --release -- probe     # rainbow pads + test blit, dump HID 12s
cargo run --release -- inhibit
cargo run --release -- restore
cargo run --release -- watch --screen   # no device; print backend model
```

`--no-inhibit` if you already killed NIHIA. `--no-restore` leaves NI agents down.

herdr 0.9.x, protocol 22. Rex API revision 4 (Rex Beta 0.1.0).

Screen text is [Inter](https://github.com/rsms/inter) (SIL OFL 1.1, `assets/fonts/Inter-LICENSE.txt`), embedded in the binary. `cargo test preview -- --ignored` renders both panels to `target/preview/*.ppm`.
