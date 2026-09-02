# herdr-kontrol

Maschine MK3 (`17cc:1600`) or Plus (`17cc:1820`) as a glance+focus surface for [herdr](https://github.com/herdrdev/herdr).

While running, this process seizes the MK3 HID interface and claims bulk IF#5 (480×272 RGB565). It kills **NIHostIntegrationAgent** and **NIHardwareAgent** so those processes cannot steal IF#5; Maschine 3.app goes dark. SIGINT/SIGTERM join the stomp thread, blank pads, then relaunch the NI agents. `--no-restore` leaves them down (you will need `herdr-kontrol restore` or a reboot if launchd has been throttled).

HID is opened with Darwin seize. IF#5 is `USBInterfaceOpen` (not Seize) — NIHIA can still steal the pipe on respawn, which is why the stomp thread exists.

Alpha. macOS arm64. Glance+focus only — no send-keys / approve.

## Map

| Hardware | Function |
|---|---|
| Pads 1–8 | Attention-sorted working set (blocked > working > done > idle). Press focuses. 9+ page via ← → (top two pad rows stay dark on page 1). |
| Groups A–H | Workspaces by `number`; press focuses |
| Dual screens | Left: selected occupant (hero/task). Right: attention queue (up to 8 rows / page) |
| Encoder | Cycle selection |
| Encoder press | `agent.focus` |
| Top buttons | Focus occupied pads 1–8 |
| ← → | Page |

Talks to `~/.config/herdr/herdr.sock` (`session.snapshot` + `events.subscribe` + `agent.focus`).

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
```

`--no-inhibit` if you already killed NIHIA. `--no-restore` leaves NI agents down.

herdr 0.8.x, protocol 19.
