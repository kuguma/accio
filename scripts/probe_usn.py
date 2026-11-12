"""非昇格で FSCTL_ENUM_USN_DATA と OpenFileById が使えるかを確かめる。"""
import ctypes, struct, sys
from ctypes import wintypes
k = ctypes.windll.kernel32
GENERIC_READ = 0x80000000
FILE_SHARE_READ, FILE_SHARE_WRITE = 1, 2
OPEN_EXISTING = 3
FILE_FLAG_BACKUP_SEMANTICS = 0x02000000
FSCTL_ENUM_USN_DATA = 0x000900B3
INVALID = ctypes.c_void_p(-1).value
k.CreateFileW.restype = ctypes.c_void_p
k.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p]
k.DeviceIoControl.argtypes = [ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
for access in (0, GENERIC_READ):
    h = k.CreateFileW(r"\\.\C:", access, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, 0, None)
    print(f"open \\\\.\\C: access={access:#x}:", "ok" if h != INVALID else f"fail err={k.GetLastError()}")
    if h == INVALID:
        continue
    inp = struct.pack("<Qqq", 0, 0, 0x7FFFFFFFFFFFFFFF)
    out = ctypes.create_string_buffer(64 * 1024)
    ret = wintypes.DWORD(0)
    ok = k.DeviceIoControl(h, FSCTL_ENUM_USN_DATA, inp, len(inp), out, len(out), ctypes.byref(ret), None)
    print("  FSCTL_ENUM_USN_DATA:", "ok bytes=%d" % ret.value if ok else "fail err=%d" % k.GetLastError())
    if ok and ret.value >= 8 + 60:
        nxt = struct.unpack_from("<Q", out, 0)[0]
        reclen, major, minor = struct.unpack_from("<IHH", out, 8)
        print("  next frn=%d first record len=%d version=%d.%d" % (nxt, reclen, major, minor))
    k.CloseHandle(h)
# OpenFileById: ルートを開き、既知ファイルの FRN から開き直す
root = k.CreateFileW(r"C:\\", 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, None)
print("open C:\\ root:", "ok" if root != INVALID else "fail")
f = k.CreateFileW(r"C:\Windows\notepad.exe", 0, FILE_SHARE_READ, None, OPEN_EXISTING, 0, None)
class BHFI(ctypes.Structure):
    _fields_ = [("attrs", wintypes.DWORD), ("ct", wintypes.FILETIME), ("at", wintypes.FILETIME), ("wt", wintypes.FILETIME), ("vsn", wintypes.DWORD), ("sizeh", wintypes.DWORD), ("sizel", wintypes.DWORD), ("links", wintypes.DWORD), ("idxh", wintypes.DWORD), ("idxl", wintypes.DWORD)]
info = BHFI()
k.GetFileInformationByHandle(ctypes.c_void_p(f), ctypes.byref(info))
frn = (info.idxh << 32) | info.idxl
print("notepad frn=%#x size=%d" % (frn, (info.sizeh << 32) | info.sizel))
k.CloseHandle(f)
class FID(ctypes.Structure):
    _fields_ = [("dwSize", wintypes.DWORD), ("Type", ctypes.c_int), ("FileId", ctypes.c_longlong), ("pad", ctypes.c_longlong)]
fid = FID(ctypes.sizeof(FID), 0, frn, 0)
k.OpenFileById.restype = ctypes.c_void_p
k.OpenFileById.argtypes = [ctypes.c_void_p, ctypes.POINTER(FID), wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD]
h2 = k.OpenFileById(ctypes.c_void_p(root), ctypes.byref(fid), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, FILE_FLAG_BACKUP_SEMANTICS)
print("OpenFileById:", "ok" if h2 and h2 != INVALID else "fail err=%d" % k.GetLastError())
if h2 and h2 != INVALID:
    i2 = BHFI(); k.GetFileInformationByHandle(ctypes.c_void_p(h2), ctypes.byref(i2))
    print("  size via id=%d attrs=%#x" % ((i2.sizeh << 32) | i2.sizel, i2.attrs))
    k.CloseHandle(h2)
