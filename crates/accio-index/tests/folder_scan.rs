//! フォルダ索引（ディレクトリ走査）: 階層・サイズ集計・ジャンクション非追従を一時ディレクトリで確認する。
use accio_index::folder::scan_folder;
use std::sync::atomic::AtomicBool;

#[test]
fn scans_tree_and_skips_junctions() {
    let base = std::env::temp_dir().join(format!("accio-folder-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("a/b")).unwrap();
    std::fs::write(base.join("a/one.txt"), "12345").unwrap();
    std::fs::write(base.join("a/b/two.bin"), vec![0u8; 1000]).unwrap();
    std::fs::write(base.join("日本語.md"), "x").unwrap();
    // ジャンクション（管理者不要）で親を指す循環を作る。作れない環境では省略。
    let junction = base.join("a/b/loop");
    let made = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J", &junction.display().to_string(), &base.display().to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let v = scan_folder(&base.display().to_string(), &AtomicBool::new(false), &|_| {}).unwrap();
    let paths: Vec<String> = (0..v.len() as u32).map(|i| v.full_path(i)).collect();
    let root = base.display().to_string();
    assert!(paths.contains(&format!("{root}\\a\\one.txt")), "{paths:?}");
    assert!(paths.contains(&format!("{root}\\a\\b\\two.bin")));
    assert!(paths.contains(&format!("{root}\\日本語.md")));
    // 配下サイズ
    let a = (0..v.len() as u32).find(|&i| v.name(i) == "a").unwrap();
    assert_eq!(v.size[a as usize], 1005);
    assert_eq!(v.size[0], 1006);
    if made {
        // ジャンクション自体は索引されるが、その配下は辿らない（循環しない）
        assert!(paths.contains(&format!("{root}\\a\\b\\loop")));
        assert!(!paths.iter().any(|p| p.contains("\\loop\\")), "{paths:?}");
        assert!(v.len() < 20);
    }
    let _ = std::fs::remove_dir_all(&base);
}
