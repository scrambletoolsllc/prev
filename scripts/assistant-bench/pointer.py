"""A pointer for the bench's headless sway, through wlr-virtual-pointer,
as a headless sway has no pointer of its own to move.

pointer.py STEP...  where a step is one of
  move X Y | click X Y | dclick X Y | drag X1 Y1 X2 Y2 | scroll X Y LINES | sleep S

It talks to the display WAYLAND_DISPLAY names, BENCH_SIZE (WIDTHxHEIGHT)
big. Its protocol code is made once from the protocols' XML, into
PROTOCOLS (a folder the bench gives it)."""

import os
import sys
import time

sys.path.insert(0, os.environ["PROTOCOLS"])
from pywayland.client import Display  # noqa: E402
from protocols.wayland import WlSeat  # noqa: E402
from protocols.wlr_virtual_pointer_unstable_v1 import (  # noqa: E402
    ZwlrVirtualPointerManagerV1,
)

WIDTH, HEIGHT = map(int, os.environ.get("BENCH_SIZE", "1600x1000").split("x"))
BTN_LEFT = 0x110

display = Display(os.environ["WAYLAND_DISPLAY"])
display.connect()
found = {}


def on_global(registry, name, interface, version):
    if interface == "wl_seat":
        found["seat"] = registry.bind(name, WlSeat, 1)
    elif interface == "zwlr_virtual_pointer_manager_v1":
        found["manager"] = registry.bind(name, ZwlrVirtualPointerManagerV1, 1)


registry = display.get_registry()
registry.dispatcher["global"] = on_global
display.roundtrip()
pointer = found["manager"].create_virtual_pointer(found["seat"])


def now():
    return int(time.monotonic() * 1000) & 0xFFFFFFFF


def flush():
    pointer.frame()
    display.flush()
    display.roundtrip()


def move(x, y):
    pointer.motion_absolute(now(), int(x), int(y), WIDTH, HEIGHT)
    flush()
    time.sleep(0.03)


def button(down):
    pointer.button(now(), BTN_LEFT, 1 if down else 0)
    flush()
    time.sleep(0.04)


args = sys.argv[1:]
i = 0
while i < len(args):
    step = args[i]
    if step == "move":
        move(args[i + 1], args[i + 2])
        i += 3
    elif step in ("click", "dclick"):
        move(args[i + 1], args[i + 2])
        for _ in range(2 if step == "dclick" else 1):
            button(True)
            button(False)
        time.sleep(0.25)
        i += 3
    elif step == "drag":
        x1, y1, x2, y2 = map(int, args[i + 1 : i + 5])
        move(x1, y1)
        button(True)
        # In steps, as a hand would, so the drag reads as one.
        for part in range(1, 13):
            move(x1 + (x2 - x1) * part / 12, y1 + (y2 - y1) * part / 12)
        button(False)
        time.sleep(0.25)
        i += 5
    elif step == "scroll":
        move(args[i + 1], args[i + 2])
        lines = int(args[i + 3])
        for _ in range(abs(lines)):
            pointer.axis_discrete(
                now(), 0, 15.0 if lines > 0 else -15.0, 1 if lines > 0 else -1
            )
            flush()
            time.sleep(0.05)
        i += 4
    elif step == "sleep":
        time.sleep(float(args[i + 1]))
        i += 2
    else:
        sys.exit(f"pointer.py: no step called {step}")
display.disconnect()
