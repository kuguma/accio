"""Everything 1.4 の ETP クライアントと同じ手順で accio の ETP サーバーを叩く検証スクリプト。
使い方: python -I scripts/etp_client.py <host> <port> "<query>" [user] [password]
"""
import socket, sys, ftplib, io

def recv_reply(f):
    """複数行応答（NNN-…/NNN …）を 1 応答として読む。"""
    lines = []
    while True:
        line = f.readline()
        if not line:
            raise EOFError
        line = line.decode("utf-8").rstrip("\r\n")
        lines.append(line)
        if len(line) >= 4 and line[:3].isdigit() and line[3] == " ":
            return lines

def cmd(s, f, text):
    s.sendall((text + "\r\n").encode("utf-8"))
    return recv_reply(f)

def main():
    host = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 2121
    query = sys.argv[3] if len(sys.argv) > 3 else "ext:md"
    user = sys.argv[4] if len(sys.argv) > 4 else "anonymous"
    password = sys.argv[5] if len(sys.argv) > 5 else ""
    s = socket.create_connection((host, port), timeout=20)
    f = s.makefile("rb")
    print("banner:", recv_reply(f)[-1])
    print("USER:", cmd(s, f, f"USER {user}")[-1])
    r = cmd(s, f, f"PASS {password}")
    print("PASS:", r[-1])
    print("FEAT:", " ".join(x.strip() for x in cmd(s, f, "FEAT")[1:-1]))
    print("OPTS:", cmd(s, f, "OPTS UTF8 ON")[-1])
    for c in ["EVERYTHING SIZE_COLUMN 1", "EVERYTHING DATE_MODIFIED_COLUMN 1", "EVERYTHING PATH_COLUMN 1", "EVERYTHING COUNT 3",
              "EVERYTHING HIDE_EMPTY_SEARCH_RESULTS 1", f"EVERYTHING SEARCH {query}", "EVERYTHING SORT SIZE_DESCENDING"]:
        print(f"{c}: {cmd(s, f, c)[-1]}")
    r = cmd(s, f, "EVERYTHING QUERY")
    print("QUERY reply:")
    for line in r:
        print("  ", line)
    print("SITE EVERYTHING OFFSET 1:", cmd(s, f, "SITE EVERYTHING OFFSET 1")[-1])
    r = cmd(s, f, "SITE EVERYTHING QUERY")
    print("QUERY offset=1 first result line:", [x for x in r if x.startswith(" FILE") or x.startswith(" FOLDER")][:1])
    print("bad sort:", cmd(s, f, "EVERYTHING SORT BOGUS")[-1])
    print("unknown:", cmd(s, f, "EVERYTHING NOPE")[-1])
    print("QUIT:", cmd(s, f, "QUIT")[-1])
    s.close()
    # 汎用 FTP クライアントでの閲覧
    ftp = ftplib.FTP()
    ftp.encoding = "utf-8"
    ftp.connect(host, port, timeout=20)
    ftp.login(user, password)
    print("PWD:", ftp.pwd())
    root = []
    ftp.retrlines("LIST", root.append)
    print("LIST /:", root[:5])
    if root:
        first = root[0].split()[-1]
        ftp.cwd(first)
        print("CWD:", ftp.pwd())
        sub = []
        ftp.retrlines("LIST", sub.append)
        print("LIST sub:", sub[:3])
        ml = []
        ftp.retrlines("MLSD", ml.append)
        print("MLSD sub:", ml[:2])
    ftp.quit()

if __name__ == "__main__":
    main()
