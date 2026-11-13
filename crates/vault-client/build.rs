//! TWOPW_EMBED_OAUTH_CLIENT=<oauth-client.json のパス> を付けてビルドすると、
//! OAuth クライアントをアプリに埋め込む（配布用インストーラー向け）。付けなければ空。
fn main() {
    println!("cargo:rerun-if-env-changed=TWOPW_EMBED_OAUTH_CLIENT");
    let out =
        std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("embedded_oauth_client.json");
    let content = match std::env::var("TWOPW_EMBED_OAUTH_CLIENT") {
        Ok(path) if !path.is_empty() => {
            println!("cargo:rerun-if-changed={path}");
            let s = std::fs::read_to_string(&path)
                .expect("TWOPW_EMBED_OAUTH_CLIENT のファイルを読めません");
            let v: serde_json::Value =
                serde_json::from_str(&s).expect("OAuth クライアントの JSON が不正です");
            assert!(
                v.get("installed").is_some(),
                "「デスクトップ アプリ」の OAuth クライアントを指定してください"
            );
            s
        }
        _ => String::new(),
    };
    std::fs::write(out, content).unwrap();
}
