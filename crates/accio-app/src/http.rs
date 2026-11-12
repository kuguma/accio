//! Everything 互換 HTTP サーバー（既定は無効。accio.ini の http_server=1 / http_port=NNNN / http_bind=）。
//!
//! `GET /?search=<q>&json=1&offset=&count=&sort=name|path|size|date_modified&ascending=1&case=&wholeword=&path=&regex=`
//! に Everything と同じ JSON（totalResults / results[]: type,name,path,size,date_modified）で応答する。
//! ファイルのダウンロード提供は行わない（安全側）。

use accio_index::Index;
use accio_query::{search, Request, SearchOpts, Sort, SortKey};
use std::sync::Arc;
use tiny_http::{Header, Method, Response, Server};

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (url_decode(k), url_decode(v)),
            None => (url_decode(kv), String::new()),
        })
        .collect()
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn handle(index: &Index, params: &[(String, String)]) -> (String, bool) {
    let get = |k: &str| params.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
    let flag = |k: &str| matches!(get(k), Some("1") | Some("true"));
    let query = get("search").or(get("s")).unwrap_or("").to_string();
    let offset: usize = get("offset").or(get("o")).and_then(|v| v.parse().ok()).unwrap_or(0);
    let count: usize = get("count").or(get("c")).and_then(|v| v.parse().ok()).unwrap_or(32);
    let json = flag("json") || flag("j");
    let sort = Sort {
        key: match get("sort") {
            Some("path") => SortKey::Path,
            Some("size") => SortKey::Size,
            Some("date_modified") => SortKey::DateModified,
            Some("date_created") => SortKey::DateCreated,
            Some("date_accessed") => SortKey::DateAccessed,
            Some("extension") => SortKey::Extension,
            Some("run_count") => SortKey::RunCount,
            _ => SortKey::Name,
        },
        ascending: get("ascending").map(|v| v != "0").unwrap_or(true),
    };
    let opts = SearchOpts { match_case: flag("case"), whole_word: flag("wholeword"), match_path: flag("path"), regex: flag("regex"), match_diacritics: flag("diacritics") };
    let vols = index.snapshot();
    let res = search(&vols, &Request { query: query.clone(), opts, sort, ..Default::default() }, &|| false).unwrap_or_default();
    let start = offset.min(res.hits.len());
    let end = (start + count).min(res.hits.len());
    let want_size = json || flag("size_column");
    let want_dm = json || flag("date_modified_column");
    let want_dc = flag("date_created_column");
    let want_attr = flag("attributes_column");
    if json {
        let mut out = String::new();
        out.push_str(&format!("{{\"totalResults\":{},\"results\":[", res.total));
        for (i, h) in res.hits[start..end].iter().enumerate() {
            let g = vols[h.vol as usize].read();
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"type\":{},\"name\":{},\"path\":{}",
                if g.is_dir(h.id) { "\"folder\"" } else { "\"file\"" },
                json_str(g.name(h.id)),
                json_str(&g.parent_path(h.id))
            ));
            if want_size {
                out.push_str(&format!(",\"size\":\"{}\"", g.size[h.id as usize]));
            }
            if want_dm {
                out.push_str(&format!(",\"date_modified\":\"{}\"", g.mtime(h.id)));
            }
            if want_dc {
                out.push_str(&format!(",\"date_created\":\"{}\"", g.ctime(h.id)));
            }
            if want_attr {
                out.push_str(&format!(",\"attributes\":\"{}\"", g.attrs[h.id as usize]));
            }
            out.push('}');
        }
        out.push_str("]}");
        (out, true)
    } else {
        let mut out = String::new();
        out.push_str("<!doctype html><html><head><meta charset=\"utf-8\"><title>accio</title><style>body{font-family:sans-serif;margin:1em}table{border-collapse:collapse}td,th{padding:2px 8px;text-align:left}tr:nth-child(even){background:#f4f4f4}.n{color:#666}</style></head><body>");
        out.push_str(&format!(
            "<form><input name=\"search\" value=\"{}\" size=\"60\" autofocus> <input type=\"submit\" value=\"検索\"> <label><input type=\"checkbox\" name=\"path\" value=\"1\"{}>パス</label> <label><input type=\"checkbox\" name=\"regex\" value=\"1\"{}>正規表現</label></form>",
            html_escape(&query),
            if opts.match_path { " checked" } else { "" },
            if opts.regex { " checked" } else { "" }
        ));
        out.push_str(&format!("<p class=\"n\">{} 件 ({:.0} ms)</p><table><tr><th>名前</th><th>パス</th><th>サイズ</th><th>更新日時</th></tr>", res.total, res.elapsed_ms));
        for h in &res.hits[start..end] {
            let g = vols[h.vol as usize].read();
            out.push_str(&format!(
                "<tr><td>{}{}</td><td class=\"n\">{}</td><td>{}</td><td>{}</td></tr>",
                if g.is_dir(h.id) { "📁 " } else { "" },
                html_escape(g.name(h.id)),
                html_escape(&g.parent_path(h.id)),
                g.size[h.id as usize],
                accio_query::datetime::format_filetime(g.mtime(h.id))
            ));
        }
        out.push_str("</table>");
        if end < res.hits.len() {
            out.push_str(&format!("<p><a href=\"?search={}&offset={}&count={}\">次へ</a></p>", html_escape(&query).replace(' ', "+"), end, count));
        }
        out.push_str("</body></html>");
        (out, false)
    }
}

/// Basic 認証（Everything の HTTP サーバーと同じく任意）。`expected` は "user:password"。
fn auth_ok(req: &tiny_http::Request, expected: &Option<String>) -> bool {
    let Some(exp) = expected else { return true };
    let Some(h) = req.headers().iter().find(|h| h.field.equiv("Authorization")) else { return false };
    let v = h.value.as_str();
    let Some(b64) = v.strip_prefix("Basic ") else { return false };
    base64_decode(b64.trim()).map(|d| String::from_utf8_lossy(&d) == exp.as_str()).unwrap_or(false)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let table = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= table(c)? << (18 - 6 * i);
        }
        let n = chunk.len();
        if n >= 2 {
            out.push((acc >> 16) as u8);
        }
        if n >= 3 {
            out.push((acc >> 8) as u8);
        }
        if n == 4 {
            out.push(acc as u8);
        }
    }
    Some(out)
}

pub fn start(index: Arc<Index>, bind: &str, port: u16, auth: Option<String>) {
    let addr = format!("{bind}:{port}");
    let server = match Server::http(&addr) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("HTTP サーバーを {addr} で開始できない: {e}");
            return;
        }
    };
    log::info!("HTTP サーバー開始 http://{addr}/");
    std::thread::Builder::new()
        .name("http".into())
        .spawn(move || {
            for req in server.incoming_requests() {
                if !auth_ok(&req, &auth) {
                    let _ = req.respond(
                        Response::from_string("authentication required")
                            .with_status_code(401)
                            .with_header(Header::from_bytes("WWW-Authenticate", "Basic realm=\"accio\"").unwrap()),
                    );
                    continue;
                }
                if *req.method() != Method::Get {
                    let _ = req.respond(Response::from_string("method not allowed").with_status_code(405));
                    continue;
                }
                let url = req.url().to_string();
                let (path, qs) = url.split_once('?').unwrap_or((&url, ""));
                if path != "/" {
                    let _ = req.respond(Response::from_string("not found").with_status_code(404));
                    continue;
                }
                let params = parse_query(qs);
                let (body, is_json) = handle(&index, &params);
                let ct = if is_json { "application/json; charset=utf-8" } else { "text/html; charset=utf-8" };
                let resp = Response::from_string(body).with_header(Header::from_bytes("Content-Type", ct).unwrap());
                let _ = req.respond(resp);
            }
        })
        .ok();
}
