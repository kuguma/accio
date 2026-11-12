"""X (Twitter) 投稿用バナー 1600x900 を PIL で描く。"""
import sys
from PIL import Image, ImageDraw, ImageFont, ImageFilter

R = r'C:\repos\github.com\kuguma\accio'
OUT = sys.argv[1]
LANG = sys.argv[2] if len(sys.argv) > 2 else 'ja'
SHOT = sys.argv[3] if len(sys.argv) > 3 else R + r'\docs\images\screenshot.png'
W, H = 1600, 900
PAPER = (244, 243, 239)
INK = (22, 22, 26)
MUTE = (110, 110, 118)
ACCENT = (200, 60, 0)

def font(path, size, index=0):
    return ImageFont.truetype(path, size, index=index)

F = r'C:\Windows\Fonts'
f_title = font(F + r'\YuGothB.ttc', 128, 1)
f_tag = font(F + r'\YuGothB.ttc', 50, 1)
f_sub = font(F + r'\YuGothM.ttc', 28, 1)
f_chip = font(F + r'\YuGothB.ttc', 30, 1)
f_url = font(F + r'\CascadiaMono.ttf', 26)

im = Image.new('RGB', (W, H), PAPER)
d = ImageDraw.Draw(im)

# 左上: ロゴ + ワードマーク
logo = Image.open(R + r'\docs\images\logo.png').convert('RGBA')
# ロゴは角丸タイル。タイルの地色を背景に溶かすため、黒の筆画だけを使う
ink_only = Image.new('RGBA', logo.size, (0, 0, 0, 0))
px = logo.load(); ip = ink_only.load()
for y in range(logo.size[1]):
    for x in range(logo.size[0]):
        r, g, b, a = px[x, y]
        lum = (r + g + b) / 3
        alpha = 0 if lum >= 232 else int(max(0, min(255, (232 - lum) * 255 / 210)) * a / 255)
        ip[x, y] = (INK[0], INK[1], INK[2], alpha)
logo_big = ink_only.resize((300, 300), Image.Resampling.LANCZOS)
im.paste(logo_big, (96, 70), logo_big)

d.text((420, 92), 'accio', font=f_title, fill=INK)
if LANG == 'ja':
    tag = 'Windows 向けの即時ファイル検索。'
    sub = 'NTFS の MFT を直接読み、USN ジャーナルで変更を追い、UI は決して止まらない。'
    chips = ['フリーズなし', '2,000 万件を 45 ms で検索', '起動 2.0 秒', 'MIT ライセンス']
else:
    tag = 'Instant file search for Windows.'
    sub = 'Reads the NTFS MFT directly, tracks changes through the USN journal, never blocks the UI.'
    chips = ['No freezes', '20 M files searched in 45 ms', 'Starts in 2.0 s', 'MIT licensed']
d.text((424, 246), tag, font=f_tag, fill=INK)

# チップ（文字は枠の縦中央に anchor で揃える）
x, y = 424, 344
ch = 58
for c in chips:
    tw = d.textlength(c, font=f_chip)
    if x + tw + 40 > W - 80:
        x = 424; y += ch + 14
    d.rounded_rectangle((x, y, x + tw + 40, y + ch), radius=ch // 2, outline=INK, width=2)
    d.text((x + 20, y + ch / 2), c, font=f_chip, fill=INK, anchor='lm')
    x += tw + 40 + 14

# 下: スクリーンショット（角丸ウィンドウ、影つき、下端にかける）
shot = Image.open(SHOT).convert('RGB')
sw = 1440
sh = int(shot.size[1] * sw / shot.size[0])
shot = shot.resize((sw, sh), Image.Resampling.LANCZOS)
# タイトルバー風の帯
bar = 36
card = Image.new('RGB', (sw, sh + bar), (232, 231, 226))
card.paste(shot, (0, bar))
cd = ImageDraw.Draw(card)
cd.text((16, 7), 'accio', font=font(F + r'\YuGothM.ttc', 18, 1), fill=MUTE)
cd.rectangle((0, bar - 1, sw, bar - 1), fill=(210, 209, 204))
radius = 18
mask = Image.new('L', card.size, 0)
ImageDraw.Draw(mask).rounded_rectangle((0, 0, sw, sh + bar), radius=radius, fill=255)
cx, cy = (W - sw) // 2, 470
shadow = Image.new('RGBA', (W, H), (0, 0, 0, 0))
sd = ImageDraw.Draw(shadow)
sd.rounded_rectangle((cx + 4, cy + 18, cx + sw - 4, cy + sh + bar + 18), radius=radius, fill=(0, 0, 0, 90))
shadow = shadow.filter(ImageFilter.GaussianBlur(22))
im.paste(shadow, (0, 0), shadow)
im.paste(card, (cx, cy), mask)
d = ImageDraw.Draw(im)
d.rounded_rectangle((cx, cy, cx + sw, cy + sh + bar), radius=radius, outline=(200, 199, 194), width=1)

# URL（右上）
url = 'github.com/kuguma/accio'
tw = d.textlength(url, font=f_url)
d.text((W - 80 - tw, 70), url, font=f_url, fill=MUTE)

im.save(OUT, optimize=True)
print('saved', OUT, im.size)
