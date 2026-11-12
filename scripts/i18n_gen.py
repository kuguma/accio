"""UI 文字列の英語対訳表 `crates/accio-app/src/i18n_en.rs` を `i18n_en.json` から生成する。

使い方: python -I -X utf8 scripts/i18n_gen.py
鍵（日本語の原文）は、コード中の `tr("…")` / `trf("…", &[…])` に渡す文字列と一致させる。
名前付きプレースホルダ `{name}` `{name:?}` `{name:.2}` は位置指定 `{}` `{:.2}` に正規化される。
"""
import json, os, re

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, 'crates', 'accio-app', 'src')

def normalize(s):
    """`{ident}`/`{ident:spec}` → `{}`/`{:spec}`（`:?` は `{}`）。`{{` `}}` はそのまま。"""
    out = []
    i = 0
    while i < len(s):
        c = s[i]
        if c == '{' and i + 1 < len(s) and s[i + 1] == '{':
            out.append('{{'); i += 2; continue
        if c == '}' and i + 1 < len(s) and s[i + 1] == '}':
            out.append('}}'); i += 2; continue
        if c == '{':
            j = s.index('}', i)
            inner = s[i + 1:j]
            if ':' in inner:
                name, spec = inner.split(':', 1)
            else:
                name, spec = inner, ''
            if spec == '?':
                spec = ''
            out.append('{' + (':' + spec if spec else '') + '}')
            i = j + 1
            continue
        out.append(c); i += 1
    return ''.join(out)

def rust_str(s):
    # JSON の値はコード中のリテラル表記（\" や \n を含む）そのものなので、そのまま引用符で包む
    return '"' + s + '"'

def main():
    src = json.load(open(os.path.join(SRC, 'i18n_en.json'), encoding='utf-8'))
    pairs = {}
    for ja, en in src.items():
        # JSON は Rust のリテラル表記（\" や \n）を文字そのものとして持つ
        k = normalize(ja)
        v = normalize(en)
        pairs[k] = v
    items = sorted(pairs.items())
    lines = ['// 自動生成: scripts/i18n_gen.py が i18n_en.json から作る。手で編集しない。', 'pub static EN: &[(&str, &str)] = &[']
    for k, v in items:
        lines.append(f'    ({rust_str(k)}, {rust_str(v)}),')
    lines.append('];')
    open(os.path.join(SRC, 'i18n_en.rs'), 'w', encoding='utf-8').write('\n'.join(lines) + '\n')
    print('entries:', len(items))

if __name__ == '__main__':
    main()
