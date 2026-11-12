use embed_manifest::{embed_manifest, manifest::{ExecutionLevel, Setting}, new_manifest};

fn main() {
    // 昇格は実行時に自前で行う（非昇格の読み取り専用モードを残すため）。long path 対応のみマニフェストで宣言。
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(
            new_manifest("accio").requested_execution_level(ExecutionLevel::AsInvoker).long_path_aware(Setting::Enabled),
        )
        .expect("manifest");
        // exe のアイコン（タスクバー・エクスプローラー・ALT+TAB）。assets/icon は MONOFORM アイコンシステムのコンセプト 022 から生成（assets/icon/build.py）
        embed_resource::compile("accio.rc", embed_resource::NONE).manifest_optional().expect("icon resource");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=accio.rc");
    println!("cargo:rerun-if-changed=assets/icon/accio.ico");
}
