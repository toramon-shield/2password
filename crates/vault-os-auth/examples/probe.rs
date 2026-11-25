//! 動作確認用：`cargo run -p vault-os-auth --example probe [create|open|remove]`
fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let label = "2password-probe";
    let salt = [7u8; 32];
    println!("method={} available={}", vault_os_auth::method().as_str(), vault_os_auth::available());
    let hex = |k: &[u8]| k.iter().take(4).map(|b| format!("{b:02x}")).collect::<String>();
    match cmd.as_str() {
        "create" => match vault_os_auth::create(label, &salt, "2password probe") {
            Ok(k) => println!("created key prefix={}", hex(&*k)),
            Err(e) => println!("create error: {e}"),
        },
        "open" => match vault_os_auth::open(label, &salt, "2password probe") {
            Ok(k) => println!("opened key prefix={}", hex(&*k)),
            Err(e) => println!("open error: {e}"),
        },
        "remove" => { vault_os_auth::remove(label); println!("removed"); }
        _ => {}
    }
}
