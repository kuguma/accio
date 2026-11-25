"""Windows live probe: cached startup, idle cost, native tray exit, and saved session."""
import argparse
import ctypes as c
from ctypes import wintypes as w
import hashlib
import json
import os
import statistics
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

u = c.WinDLL("user32", use_last_error=True)
k = c.WinDLL("kernel32", use_last_error=True)
u.GetWindowThreadProcessId.argtypes = [w.HWND, c.POINTER(w.DWORD)]
u.GetClassNameW.argtypes = [w.HWND, w.LPWSTR, c.c_int]
u.GetWindowTextW.argtypes = [w.HWND, w.LPWSTR, c.c_int]
u.IsWindowVisible.argtypes = [w.HWND]
u.PostMessageW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM]
k.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
k.OpenProcess.restype = w.HANDLE
k.CloseHandle.argtypes = [w.HANDLE]
k.GetProcessTimes.argtypes = [w.HANDLE] + [c.POINTER(w.FILETIME)] * 4


class IoCounters(c.Structure):
    _fields_ = [(n, c.c_ulonglong) for n in (
        "read_ops", "write_ops", "other_ops", "read_bytes", "write_bytes", "other_bytes")]


k.GetProcessIoCounters.argtypes = [w.HANDLE, c.POINTER(IoCounters)]


class ThreadEntry(c.Structure):
    _fields_ = [("size", w.DWORD), ("usage", w.DWORD), ("tid", w.DWORD),
                ("pid", w.DWORD), ("base", w.LONG), ("delta", w.LONG), ("flags", w.DWORD)]


k.CreateToolhelp32Snapshot.argtypes = [w.DWORD, w.DWORD]
k.CreateToolhelp32Snapshot.restype = w.HANDLE
k.Thread32First.argtypes = k.Thread32Next.argtypes = [w.HANDLE, c.POINTER(ThreadEntry)]
k.OpenThread.argtypes = [w.DWORD, w.BOOL, w.DWORD]
k.OpenThread.restype = w.HANDLE
k.GetThreadPriority.argtypes = [w.HANDLE]
k.GetThreadDescription.argtypes = [w.HANDLE, c.POINTER(w.LPWSTR)]
k.LocalFree.argtypes = [c.c_void_p]


def thread_priorities(pid):
    out = []
    snapshot = k.CreateToolhelp32Snapshot(4, 0)
    try:
        entry = ThreadEntry(size=c.sizeof(ThreadEntry))
        ok = k.Thread32First(snapshot, c.byref(entry))
        while ok:
            if entry.pid == pid:
                handle = k.OpenThread(0x0800, False, entry.tid)
                if handle:
                    try:
                        name = w.LPWSTR()
                        k.GetThreadDescription(handle, c.byref(name))
                        out.append({"name": name.value or "", "priority": k.GetThreadPriority(handle)})
                        if name:
                            k.LocalFree(c.cast(name, c.c_void_p))
                    finally:
                        k.CloseHandle(handle)
            ok = k.Thread32Next(snapshot, c.byref(entry))
    finally:
        k.CloseHandle(snapshot)
    return out


def bench_search():
    import ipc3_client as ipc
    ipc.kernel32.CreateFileW.restype = w.HANDLE
    ipc.kernel32.CreateFileW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, c.c_void_p, w.DWORD, w.DWORD, w.HANDLE]
    ipc.kernel32.ReadFile.argtypes = [w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p]
    ipc.kernel32.WriteFile.argtypes = ipc.kernel32.ReadFile.argtypes
    ipc.kernel32.CloseHandle.argtypes = [w.HANDLE]
    handle, _ = ipc.connect("resident-probe")
    results = {}
    try:
        for query in ["report", "windows update", "ext:pdf", "*.dll", r"regex:^lib.*\.a$"]:
            times = []
            for _ in range(6):
                started = time.perf_counter()
                rc, result = ipc.search(handle, query, count=5, props=[(0, 0)])
                times.append((time.perf_counter() - started) * 1000)
                if rc != 200:
                    raise RuntimeError(f"search {query!r}: {rc}")
            results[query] = {"median_ms": statistics.median(times[1:]), "samples_ms": times[1:],
                              "hits": result["folders"] + result["files"]}
    finally:
        ipc.kernel32.CloseHandle(handle)
    return results


def windows(pid):
    out = []
    callback_type = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)

    @callback_type
    def visit(hwnd, _):
        owner = w.DWORD()
        u.GetWindowThreadProcessId(hwnd, c.byref(owner))
        if owner.value == pid:
            cls, title = c.create_unicode_buffer(256), c.create_unicode_buffer(256)
            u.GetClassNameW(hwnd, cls, 256)
            u.GetWindowTextW(hwnd, title, 256)
            out.append((hwnd, cls.value, title.value, bool(u.IsWindowVisible(hwnd))))
        return True

    u.EnumWindows(visit, 0)
    return out


def usage(handle):
    ts = [w.FILETIME() for _ in range(4)]
    if not k.GetProcessTimes(handle, *(c.byref(t) for t in ts)):
        raise c.WinError(c.get_last_error())
    cpu = sum((t.dwHighDateTime << 32) | t.dwLowDateTime for t in ts[2:]) / 1e7
    io = IoCounters()
    if not k.GetProcessIoCounters(handle, c.byref(io)):
        raise c.WinError(c.get_last_error())
    return {"cpu_seconds": cpu, **{n: getattr(io, n) for n, _ in io._fields_}}


def main():
    p = argparse.ArgumentParser(__doc__)
    p.add_argument("exe", type=Path, nargs="?", default=Path(os.environ.get("CARGO_TARGET_DIR", "target")) / "release" / "accio.exe")
    p.add_argument("--cache", type=Path, default=Path(os.environ["LOCALAPPDATA"]) / "accio")
    p.add_argument("--startup-seconds", type=float, default=25)
    p.add_argument("--idle-seconds", type=float, default=20)
    p.add_argument("--exit-via", choices=["tray", "cli", "ipc"], default="tray")
    p.add_argument("--live-usn", dest="elevated", action="store_true")
    p.add_argument("--visible", action="store_true")
    p.add_argument("--floating", action="store_true")
    p.add_argument("--bench-search", action="store_true")
    p.add_argument("--screenshot", type=Path)
    p.add_argument("--output", type=Path, required=True)
    args = p.parse_args()
    if args.elevated and not c.windll.shell32.IsUserAnAdmin():
        p.error("--live-usn requires an already elevated terminal")
    report = {"exe": str(args.exe.resolve()), "time": time.time(), "elevated": args.elevated,
              "sha256": hashlib.sha256(args.exe.read_bytes()).hexdigest(),
              "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=Path(__file__).resolve().parent, text=True).strip()}
    proc, handle = None, None
    with tempfile.TemporaryDirectory(prefix="accio-resident-") as tmp:
        data = Path(tmp)
        for cache in args.cache.glob("*.accio"):
            # Copies protect the user's index even when probing elevated updates.
            shutil.copyfile(cache, data / cache.name)
        (data / "accio.ini").write_text(f"lang=en\npinned={int(not args.floating)}\nsession.tab.0=report\n", encoding="utf-8")
        env = dict(os.environ, ACCIO_DATA_DIR=str(data))
        if not args.elevated:
            env["ACCIO_NO_ELEVATE"] = "1"
        try:
            t0 = time.perf_counter()
            if args.screenshot:
                args.screenshot = args.screenshot.resolve()
                env["ACCIO_UI_SCRIPT"] = f"wait:600;shot:{args.screenshot};dump:{args.screenshot.with_suffix('.txt')}"
            cmd = [str(args.exe.resolve()), "-instance", "resident-probe"]
            if not args.visible:
                cmd.append("-startup")
            proc = subprocess.Popen(cmd, env=env)
            handle = k.OpenProcess(0x1000, False, proc.pid)
            if not handle:
                raise c.WinError(c.get_last_error())
            samples = []
            end = t0 + args.startup_seconds + args.idle_seconds
            while time.perf_counter() < end:
                if proc.poll() is not None:
                    raise RuntimeError(f"unexpected exit: {proc.returncode}")
                samples.append({"seconds": time.perf_counter() - t0, **usage(handle)})
                if "startup_threads" not in report and samples[-1]["seconds"] > 1.5:
                    report["startup_threads"] = thread_priorities(proc.pid)
                time.sleep(0.25)
            report["samples"] = samples
            idle = [s for s in samples if s["seconds"] >= args.startup_seconds]
            report["idle"] = {key: idle[-1][key] - idle[0][key] for key in idle[0]}
            report["idle_threads"] = thread_priorities(proc.pid)
            if args.bench_search:
                report["queries"] = bench_search()
                report["search_threads"] = thread_priorities(proc.pid)
            if args.visible:
                main_window = next(hwnd for hwnd, cls, _, _ in windows(proc.pid) if cls == "Window Class")
                u.PostMessageW(main_window, 0x10, 0, 0)
                time.sleep(1)
            report["windows_before_exit"] = [(cls, title, visible) for _, cls, title, visible in windows(proc.pid)]
            t_exit = time.perf_counter()
            if args.exit_via == "tray":
                tray = next(hwnd for hwnd, cls, _, _ in windows(proc.pid) if cls == "tray_icon_app")
                # muda's native menu allocates root=1000, show=1001, new=1002, exit=1003.
                if not u.PostMessageW(tray, 0x111, 1003, 0):
                    raise c.WinError(c.get_last_error())
            elif args.exit_via == "ipc":
                ipc_window = next(hwnd for hwnd, cls, _, _ in windows(proc.pid) if cls.startswith("EVERYTHING_TASKBAR_NOTIFICATION"))
                u.PostMessageW(ipc_window, 0x400, 4, 0)
            else:
                subprocess.run([str(args.exe.resolve()), "-instance", "resident-probe", "-exit"], env=env, timeout=10, check=True)
            try:
                proc.wait(timeout=30)
                report["exit_seconds"] = time.perf_counter() - t_exit
                report["exit_code"] = proc.returncode
            except subprocess.TimeoutExpired:
                report["exit_timed_out"] = True
                report["windows_after_exit_request"] = [(cls, title, visible) for _, cls, title, visible in windows(proc.pid)]
            report["session_saved"] = "session.active=" in (data / "accio.ini").read_text(encoding="utf-8")
            if args.screenshot:
                report["screenshot_saved"] = args.screenshot.is_file()
        finally:
            if proc and proc.poll() is None:
                proc.kill()
                proc.wait(timeout=10)
            if handle:
                k.CloseHandle(handle)
            log = data / "accio.log"
            if log.exists():
                report["log"] = log.read_text(encoding="utf-8", errors="replace")
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps({key: value for key, value in report.items() if key not in ("samples", "log", "startup_threads", "idle_threads", "search_threads")}, indent=2))
    if (report.get("exit_timed_out") or report.get("exit_code") != 0 or not report.get("session_saved")
            or report.get("screenshot_saved") is False
            or any(cls == "Window Class" and visible for cls, _, visible in report.get("windows_before_exit", []))):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
