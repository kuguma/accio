# accio icon

MONOFORM（モノクロ・パラメトリックなアイコンシステム）のコンセプト **022 A-PULL** を `t = 0.516, β = 0.153` で固定したもの。
accio の「A」を姿現しの渦で歪める: 中心ほど強く捻れ、外縁で元の形に戻る。

- ω(r) = 13·t·(1 − r/R)^1.2 · (1 + env·0.35·sin(θ − ψ))、R = 84/200、pull = 0.5·t·(1 − r/R)
- ψ = 2π·β、env = sin(π·β)
- 余白と重心: マーク占有率 66%、bbox 中心をタイル中心へ、面積重心との差の半分（上限 ±16 px/1024）だけ光学補正
- 墨 #16161a のマークを紙 #f4f3ef のタイル（22.37% スーパー楕円）に乗せる

```
build.py                                   · 生成スクリプト（python -I -X utf8 build.py、Pillow が必要）
master.v1.0.0.svg                          · 1024 マスター（紙タイル＋墨マーク）
master.v1.0.0.transparent.svg              · マークのみ
accio@{32,256}.png                         · ウィンドウ/タスクバー用
accio.ico                                  · exe リソース（16/24/32/48/256）
tray/accio-tray-{tile,light,dark}@{16,20,24,32}.png · タスクトレイ用
```
