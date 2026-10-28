#!/usr/bin/env python3
"""Fake an OSC 7501 permission prompt in this Rex pane and echo every key typed into it.

The reporting process is also the foreground reader, as with a real agent, so
Play / Shift+Play / Stop should light up and show here as '\\r' / '2' / '\\x1b'.
Ctrl-C exits without clearing the record. Rex drops a blocked record when its
owner exits, so the pad should leave blocked; if the record lingers, Play/Stop
must still go dark (owner no longer in the foreground). `--clear` removes every
record on the pane, e.g. one a shell `printf` left behind.
"""

import base64
import os
import sys
import termios
import tty


def osc(fields: str) -> None:
    sys.stdout.write(f"\033]7501;{fields}\033\\")
    sys.stdout.flush()


if "--clear" in sys.argv:
    osc("state=clear")
    sys.exit(0)

msg = base64.b64encode(b"Allow fake-prompt test?").decode()
osc(f"state=blocked:kind=permission:app=fake-prompt:msg={msg}")
print("fake permission prompt up; press Play / Shift+Play / Stop (Ctrl-C quits)")
saved = termios.tcgetattr(0)
try:
    tty.setcbreak(0)
    while True:
        print(repr(os.read(0, 16)), flush=True)
except KeyboardInterrupt:
    pass
finally:
    termios.tcsetattr(0, termios.TCSADRAIN, saved)
