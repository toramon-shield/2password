//! 2password の暗号・データ形式コア。
//!
//! - I/O（ファイル・ネットワーク・時計）を持たない。呼び出し側がバイト列と時刻を渡す。
//! - 暗号プリミティブは RustCrypto の監査済みクレートのみを使い、ここでは組み合わせだけを書く。
#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

pub mod account;
pub mod base32;
pub mod envelope;
pub mod error;
pub mod generator;
pub mod kdf;
pub mod rng;
pub mod secret_key;
pub mod vault;

pub use account::{AccountFile, NewAccount, VaultKey, is_valid_vault_id};
pub use envelope::{Header, Kind};
pub use error::{Error, Result};
pub use kdf::{Auk, KdfParams};
pub use secrecy::{ExposeSecret, SecretString};
pub use secret_key::{AccountId, SecretKey};
pub use vault::{Item, Passkey, Vault};
