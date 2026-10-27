#!/usr/bin/env python3
"""Run one command while holding one of this machine's Codewhale build slots.

    scripts/build-lock.py <lock-file> -- <command> [args...]

Several agents share one checkout and one machine. Cargo's own lock is per
target directory, so builds into different target dirs still run concurrently
and can exhaust memory. This holds an exclusive flock on one of
CODEWHALE_BUILD_SLOTS lock files (default 2: `<lock-file>`, `<lock-file>.1`,
...) under the persistent cache root for the lifetime of the command, and says
who holds every slot while waiting. Slot 0 is `<lock-file>` itself, so an older
single-slot copy of this script still excludes the build in that slot.

The command runs as a child, never via exec: a daemon it spawns (an sccache
server, say) must not inherit the descriptor and pin the lock after the build
exits. Nested invocations see CODEWHALE_BUILD_LOCK_HELD and run unlocked.

Known limitation: advisory only. Cargo started outside scripts/dev-cargo.sh
does not take the lock, and on a platform without fcntl (Windows) the
command runs unlocked after a warning.
"""

import os
import signal
import subprocess
import sys
import time


def main() -> int:
    if len(sys.argv) < 4 or sys.argv[2] != "--":
        print("usage: build-lock.py <lock-file> -- <command> [args...]", file=sys.stderr)
        return 2
    lock_path, command = sys.argv[1], sys.argv[3:]
    if os.environ.get("CODEWHALE_BUILD_LOCK_HELD") or os.environ.get(
        "CODEWHALE_BUILD_LOCK", "1"
    ) in ("0", "false", "no", "off", "never", "disabled"):
        status = subprocess.call(command)
        return 128 - status if status < 0 else status
    env = dict(os.environ, CODEWHALE_BUILD_LOCK_HELD="1")
    try:
        import fcntl
    except ImportError:
        print("build-lock: fcntl unavailable; running without the machine build lock", file=sys.stderr)
        return subprocess.call(command, env=env)

    try:
        slots = max(1, int(os.environ.get("CODEWHALE_BUILD_SLOTS", "2")))
    except ValueError:
        print("build-lock: CODEWHALE_BUILD_SLOTS must be a positive integer", file=sys.stderr)
        return 2
    os.makedirs(os.path.dirname(lock_path) or ".", exist_ok=True)
    fds = [
        os.open(lock_path if slot == 0 else f"{lock_path}.{slot}", os.O_RDWR | os.O_CREAT, 0o600)
        for slot in range(slots)
    ]

    def try_slots():
        for slot_fd in fds:
            try:
                fcntl.flock(slot_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return slot_fd
            except BlockingIOError:
                continue
        return None

    fd = try_slots()
    if fd is None:
        holders = "; ".join(
            os.pread(slot_fd, 512, 0).decode(errors="replace").strip() or "holder unknown"
            for slot_fd in fds
        )
        print(
            f"build-lock: all {slots} build slots are busy ({holders}); "
            "set CODEWHALE_BUILD_LOCK=0 to skip",
            file=sys.stderr,
            flush=True,
        )
        started = time.monotonic()
        while fd is None:
            time.sleep(1)
            fd = try_slots()
        print(
            f"build-lock: acquired after {time.monotonic() - started:.0f}s",
            file=sys.stderr,
            flush=True,
        )
    for slot_fd in fds:
        if slot_fd != fd:
            os.close(slot_fd)
    os.ftruncate(fd, 0)
    summary = " ".join(command[:5])
    os.pwrite(fd, f"pid {os.getpid()} in {os.getcwd()}: {summary}\n".encode(), 0)

    child = subprocess.Popen(command, env=env)
    for forwarded in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(forwarded, lambda signum, _frame: child.send_signal(signum))
    # SIGINT reaches the whole foreground process group already; only keep
    # waiting so the lock outlives the child's own shutdown.
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    status = child.wait()
    return 128 - status if status < 0 else status


if __name__ == "__main__":
    sys.exit(main())
