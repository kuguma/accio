"""accio アイコンの書き出し。コンセプト 022 A-PULL を t=0.516, β=0.153 で固定し、
余白と重心を整えてから、マスター SVG・PNG 各サイズ・Windows ICO を生成する。

使い方: python -I -X utf8 build.py
出力: このフォルダ（master.v1.0.0.svg, master.v1.0.0.transparent.svg, accio@{size}.png, accio.ico, tray/*.png）
"""
import math, os, struct
from PIL import Image, ImageDraw

T, BETA = 0.516, 0.153
INK, PAPER = (0x16, 0x16, 0x1a), (0xf4, 0xf3, 0xef)
OUT = os.path.dirname(os.path.abspath(__file__))

# ---- 022 A-PULL の幾何（accio-v2.html と同じ式）----
C = 100.0
def sample(p0, p1, n):
    return [(p0[0] + (p1[0] - p0[0]) * i / n, p0[1] + (p1[1] - p0[1]) * i / n) for i in range(n + 1)]
def dense(poly, n=60):
    out = []
    for i in range(len(poly)):
        out += sample(poly[i], poly[(i + 1) % len(poly)], n)[:n]
    return out
def vortex(pt, t, b, k, p, pull):
    psi, env = 2 * math.pi * b, math.sin(math.pi * b)
    dx, dy = pt[0] - C, pt[1] - C
    r, th = math.hypot(dx, dy), math.atan2(dy, dx)
    R = 84.0
    u = max(0.0, 1 - r / R)
    w = k * t * (u ** p) * (1 + env * 0.35 * math.sin(th - psi))
    r2 = r * (1 - pull * t * u)
    return (C + r2 * math.cos(th + w), C + r2 * math.sin(th + w))
OUTER = [(100, 44), (150, 158), (124, 158), (114, 132), (86, 132), (76, 158), (50, 158)]
HOLE = [(100, 80), (108, 112), (92, 112)]
outer = [vortex(q, T, BETA, 13, 1.2, 0.5) for q in dense(OUTER)]
hole = [vortex(q, T, BETA, 13, 1.2, 0.5) for q in dense(HOLE)]

# ---- 余白と重心の調整 ----
# マーク占有率（MONOFORM: 52–72%）を 66% に、バウンディングボックスの中心をタイル中心へ。
# 面積重心も計算し、幾何中心との差の半分だけ寄せる（光学的な中央揃え、±16/1024 以内）。
xs = [p[0] for p in outer]; ys = [p[1] for p in outer]
bw, bh = max(xs) - min(xs), max(ys) - min(ys)
bcx, bcy = (max(xs) + min(xs)) / 2, (max(ys) + min(ys)) / 2
def centroid(poly):
    a = cx = cy = 0.0
    for i in range(len(poly)):
        x0, y0 = poly[i]; x1, y1 = poly[(i + 1) % len(poly)]
        cr = x0 * y1 - x1 * y0
        a += cr; cx += (x0 + x1) * cr; cy += (y0 + y1) * cr
    a *= 0.5
    return (cx / (6 * a), cy / (6 * a))
gcx, gcy = centroid(outer)
# 光学シフトは MONOFORM の許容 ±16/1024 px に収める
COVER = 0.66
_scale0 = 1024 * COVER / max(bw, bh)
_lim = 16 / _scale0
tcx = bcx + max(-_lim, min(_lim, (gcx - bcx) * 0.5))
tcy = bcy + max(-_lim, min(_lim, (gcy - bcy) * 0.5))
COVER = 0.66
scale = 1024 * COVER / max(bw, bh)
def tx(p):
    return ((p[0] - tcx) * scale + 512, (p[1] - tcy) * scale + 512)
outer_m = [tx(p) for p in outer]
hole_m = [tx(p) for p in hole]
print(f"bbox {bw:.1f}x{bh:.1f} → scale {scale:.3f}, cover {COVER:.0%}, optical shift ({(tcx-bcx)*scale:+.1f}, {(tcy-bcy)*scale:+.1f}) px / 1024")

# ---- タイル（22.37% スーパー楕円、n≈5）----
def squircle(cx, cy, half, n=5.0, steps=256):
    pts = []
    for i in range(steps):
        a = 2 * math.pi * i / steps
        ca, sa = math.cos(a), math.sin(a)
        pts.append((cx + half * math.copysign(abs(ca) ** (2 / n), ca), cy + half * math.copysign(abs(sa) ** (2 / n), sa)))
    return pts

def rgb(c):
    return '#%02x%02x%02x' % c
def path_d(poly):
    return 'M' + ' L'.join(f'{x:.2f},{y:.2f}' for x, y in poly) + ' Z'

# ---- SVG ----
tile = squircle(512, 512, 512)
mark = f'<path d="{path_d(outer_m)} {path_d(hole_m)}" fill="{rgb(INK)}" fill-rule="evenodd"/>'
svg_tile = f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024"><path d="{path_d(tile)}" fill="{rgb(PAPER)}"/>{mark}</svg>'
svg_mark = f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">{mark}</svg>'
svg_mark_light = svg_mark.replace(rgb(INK), rgb(PAPER))
open(os.path.join(OUT, 'master.v1.0.0.svg'), 'w', encoding='utf-8').write(svg_tile)
open(os.path.join(OUT, 'master.v1.0.0.transparent.svg'), 'w', encoding='utf-8').write(svg_mark)
open(os.path.join(OUT, 'mark-on-dark.svg'), 'w', encoding='utf-8').write(svg_mark_light)

# ---- ラスタ（4 倍スーパーサンプリング）----
SS = 4
def render(size, with_tile, ink_color, tile_color=None, square_tile=False):
    big = size * SS
    img = Image.new('RGBA', (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    s = big / 1024
    if with_tile:
        if square_tile:
            d.rectangle([0, 0, big, big], fill=tile_color + (255,))
        else:
            d.polygon([(x * s, y * s) for x, y in tile], fill=tile_color + (255,))
    # A（穴あき）: 外形を塗り、穴をタイル色（透明なら抜き）で戻す
    d.polygon([(x * s, y * s) for x, y in outer_m], fill=ink_color + (255,))
    hole_fill = (tile_color + (255,)) if with_tile else (0, 0, 0, 0)
    hd = ImageDraw.Draw(img)
    hd.polygon([(x * s, y * s) for x, y in hole_m], fill=hole_fill)
    return img.resize((size, size), Image.Resampling.LANCZOS)

sizes = [16, 20, 24, 32, 48, 64, 128, 256, 512, 1024]
pngs = {}
for sz in sizes:
    im = render(sz, True, INK, PAPER)
    im.save(os.path.join(OUT, f'accio@{sz}.png'))
    pngs[sz] = im
# トレイ用（マークのみ・背景透過。暗いタスクバー用に白、明るい用に墨）
os.makedirs(os.path.join(OUT, 'tray'), exist_ok=True)
for sz in [16, 20, 24, 32]:
    render(sz, False, PAPER).save(os.path.join(OUT, 'tray', f'accio-tray-light@{sz}.png'))
    render(sz, False, INK).save(os.path.join(OUT, 'tray', f'accio-tray-dark@{sz}.png'))
    render(sz, True, INK, PAPER).save(os.path.join(OUT, 'tray', f'accio-tray-tile@{sz}.png'))

# ---- ICO（16/24/32/48/256、PNG 圧縮エントリ）----
ico_sizes = [16, 24, 32, 48, 256]
entries = []
for sz in ico_sizes:
    import io
    buf = io.BytesIO(); pngs[sz].save(buf, 'PNG'); entries.append((sz, buf.getvalue()))
with open(os.path.join(OUT, 'accio.ico'), 'wb') as f:
    f.write(struct.pack('<HHH', 0, 1, len(entries)))
    offset = 6 + 16 * len(entries)
    for sz, data in entries:
        f.write(struct.pack('<BBBBHHII', sz % 256, sz % 256, 0, 0, 1, 32, len(data), offset))
        offset += len(data)
    for _, data in entries:
        f.write(data)
print('written:', sorted(os.listdir(OUT)))
