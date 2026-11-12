"""IPC v3 の索引ジャーナル（GET_JOURNAL_INFO=28 / READ_JOURNAL=29）を SDK3 と同じ順序で読む検証スクリプト。
使い方: python -I scripts/ipc3_journal.py <instance> [max_records]
"""
import ctypes, struct, sys, os
sys.path.insert(0, os.path.dirname(__file__))
from ipc3_client import connect, write_all, read_exact, call, R, kernel32

FLAGS = 0x3FFF  # すべて要求
KIND = {1: "FOLDER_CREATE", 2: "FOLDER_DELETE", 3: "FOLDER_RENAME", 4: "FOLDER_MOVE", 5: "FOLDER_MODIFY", 6: "FILE_CREATE", 7: "FILE_DELETE", 8: "FILE_RENAME", 9: "FILE_MOVE", 10: "FILE_MODIFY"}

def main():
    inst = sys.argv[1] if len(sys.argv) > 1 else "v3"
    maxn = int(sys.argv[2]) if len(sys.argv) > 2 else 3
    h, name = connect(inst)
    rc, d = call(h, 28)
    jid, first, nxt, size, maxsize = struct.unpack("<QQQQQ", d)
    print(f"journal info: rc={rc} id={jid} first={first} next={nxt} size={size} max={maxsize}")
    # READ_JOURNAL は終わらないストリーム: 100 メッセージを受けて必要件数読んだら切る
    write_all(h, struct.pack("<II", 29, 20) + struct.pack("<QQI", jid, first, FLAGS))
    buf = b""
    got = 0
    while got < maxn:
        rcode, sz = struct.unpack("<II", read_exact(h, 8))
        if rcode not in (100, 200):
            print("error rc", rcode); break
        buf += read_exact(h, sz) if sz else b""
        r = R(buf)
        while r.p < len(buf) and got < maxn:
            start = r.p
            try:
                kind = r.u8()
                cid = r.u64(); ts = r.u64(); sts = r.u64(); opdm = r.u64()
                old_path = r.s(); old_name = r.s()
                size = created = modified = accessed = None; attrs = 0
                if kind in (6, 10, 8, 9, 1, 5, 3, 4):
                    if kind in (6, 10, 8, 9): size = r.u64()
                    created = r.u64(); modified = r.u64(); accessed = r.u64(); attrs = r.u32()
                new_path = new_name = ""
                if kind in (8, 3): new_name = r.s()
                if kind in (9, 4): r.u64(); new_path = r.s(); new_name = r.s()
            except (IndexError, struct.error):
                r.p = start; break
            print(f"  #{cid} {KIND.get(kind, kind)} {old_path}\\{old_name} -> {new_path}\\{new_name} size={size} attrs={attrs}")
            got += 1
        buf = buf[r.p:]
        if rcode == 200: break
    kernel32.CloseHandle(h)

if __name__ == "__main__":
    main()
