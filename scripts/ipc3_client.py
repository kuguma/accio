"""Everything IPC v3 の最小クライアント（SDK3 と同じ符号化）。accio の ipc3 サーバー検証用。"""
import ctypes, os, struct, sys

kernel32 = ctypes.windll.kernel32
GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
OPEN_EXISTING = 3

def session_id():
    sid = ctypes.c_uint32(0)
    kernel32.ProcessIdToSessionId(kernel32.GetCurrentProcessId(), ctypes.byref(sid))
    return sid.value

def connect(instance):
    name = f"\\\\.\\PIPE\\Everything IPC Session {session_id()}"
    if instance:
        name += f" ({instance})"
    h = kernel32.CreateFileW(name, GENERIC_READ | GENERIC_WRITE, 0, None, OPEN_EXISTING, 0, None)
    if h == ctypes.c_void_p(-1).value or h == -1:
        raise OSError(f"cannot open {name}: {ctypes.GetLastError()}")
    return h, name

def write_all(h, data):
    n = ctypes.c_uint32(0)
    if not kernel32.WriteFile(h, data, len(data), ctypes.byref(n), None):
        raise OSError("WriteFile")

def read_exact(h, size):
    out = b""
    while len(out) < size:
        buf = ctypes.create_string_buffer(size - len(out))
        n = ctypes.c_uint32(0)
        if not kernel32.ReadFile(h, buf, size - len(out), ctypes.byref(n), None):
            raise OSError("ReadFile")
        if n.value == 0:
            raise OSError("EOF")
        out += buf.raw[: n.value]
    return out

def call(h, code, payload=b""):
    # SDK3 はヘッダとペイロードを別々に書く
    write_all(h, struct.pack("<II", code, len(payload)))
    if payload:
        write_all(h, payload)
    data = b""
    while True:
        rc, size = struct.unpack("<II", read_exact(h, 8))
        data += read_exact(h, size) if size else b""
        if rc == 100:
            continue
        return rc, data

def vlq(v):
    if v < 0xFF:
        return bytes([v])
    v -= 0xFF
    if v < 0xFFFF:
        return b"\xff" + struct.pack("<H", v)
    v -= 0xFFFF
    if v < 0xFFFFFFFF:
        return b"\xff\xff\xff" + struct.pack("<I", v)
    v -= 0xFFFFFFFF
    return b"\xff\xff\xff\xff\xff\xff\xff" + struct.pack("<Q", v)

class R:
    def __init__(self, b): self.b, self.p = b, 0
    def u8(self): v = self.b[self.p]; self.p += 1; return v
    def u32(self): v = struct.unpack_from("<I", self.b, self.p)[0]; self.p += 4; return v
    def u64(self): v = struct.unpack_from("<Q", self.b, self.p)[0]; self.p += 8; return v
    def vlq(self):
        a = self.u8()
        if a < 0xFF: return a
        b = struct.unpack_from("<H", self.b, self.p)[0]; self.p += 2
        if b < 0xFFFF: return 0xFF + b
        c = self.u32()
        if c < 0xFFFFFFFF: return 0xFF + 0xFFFF + c
        return 0xFF + 0xFFFF + 0xFFFFFFFF + self.u64()
    def s(self): n = self.vlq(); v = self.b[self.p:self.p+n].decode("utf-8"); self.p += n; return v
    def size_t(self, wide): return self.u64() if wide else self.u32()

def search(h, text, flags=0x4000, offset=0, count=10, sorts=(), props=()):
    pk = struct.pack("<I", flags) + vlq(len(text.encode())) + text.encode()
    wide = bool(flags & 0x4000)
    pk += struct.pack("<QQ" if wide else "<II", offset, count)
    pk += vlq(len(sorts)) + b"".join(struct.pack("<II", *s) for s in sorts)
    pk += vlq(len(props)) + b"".join(struct.pack("<II", *p) for p in props)
    rc, data = call(h, 7, pk)
    if rc != 200:
        return rc, None
    r = R(data)
    vf = r.u32(); wide = bool(vf & 0x4000)
    folders = r.size_t(wide); files = r.size_t(wide)
    total_size = r.u64() if vf & 0x800 else None
    off = r.size_t(wide); n = r.size_t(wide)
    ns = r.vlq(); srt = [(r.u32(), r.u32()) for _ in range(ns)]
    np_ = r.vlq(); pr = [(r.u32(), r.u32(), r.u8()) for _ in range(np_)]
    items = []
    for _ in range(n):
        fl = r.u8(); vals = []
        for (pid, pf, vt) in pr:
            if pf & 3 or vt in (8, 9, 10, 14, 15): vals.append(r.s())
            elif vt == 3: vals.append(r.u32())
            elif vt == 5: vals.append(r.u64())
            elif vt == 11: vals.append(r.size_t(wide))
            elif vt == 0: vals.append(None)
            else: raise ValueError(f"vt {vt}")
        items.append((fl, vals))
    return rc, dict(flags=vf, folders=folders, files=files, total_size=total_size, offset=off, count=n, sorts=srt, props=pr, items=items)

if __name__ == "__main__":
    inst = sys.argv[1] if len(sys.argv) > 1 else "v3"
    q = sys.argv[2] if len(sys.argv) > 2 else "ext:md"
    h, name = connect(inst)
    print("pipe:", name)
    for code, label in [(0, "pipe version"), (1, "major"), (2, "minor"), (8, "db loaded"), (5, "target machine")]:
        rc, d = call(h, code); print(f"{label}: rc={rc} value={struct.unpack('<I', d)[0] if len(d)==4 else d}")
    rc, d = call(h, 6, b"date-modified"); print("find property date-modified:", struct.unpack('<I', d)[0])
    rc, d = call(h, 11, struct.pack("<I", 2)); print("property 2 name:", rc, d.decode())
    rc, d = call(h, 999); print("invalid command rc:", rc)
    rc, res = search(h, q, flags=0x4000 | 0x800, count=5, sorts=[(2, 1)], props=[(0, 0), (1, 0), (2, 0), (5, 0), (8, 0), (240, 0), (2, 1), (0, 2)])
    print("search rc:", rc)
    if res:
        print({k: v for k, v in res.items() if k != "items"})
        for fl, vals in res["items"]:
            print(" ", "DIR" if fl & 1 else "   ", vals)
    # 32bit SIZE_T と viewport
    rc, res = search(h, q, flags=0, offset=1, count=2, props=[(0, 0)])
    print("32bit offset=1 count=2:", rc, res and [v[1][0] for v in res["items"]], "folders+files=", res and (res["folders"] + res["files"]))
    probe_dir = sys.argv[3] if len(sys.argv) > 3 else os.environ.get("SystemRoot", "C:\\Windows")
    rc, d = call(h, 18, probe_dir.encode()); print("folder size:", rc, struct.unpack("<Q", d)[0] if len(d) == 8 else d)
    rc, d = call(h, 19, (probe_dir + "\\notepad.exe").encode()); print("file attributes:", rc, struct.unpack("<I", d)[0] if len(d) == 4 else d)
    rc, d = call(h, 21, probe_dir.encode()); print("find first file:", rc, len(d), "bytes")
    kernel32.CloseHandle(h)
