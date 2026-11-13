//! 2password のクライアント共通部分（CLI と GUI の両方が使う）。
//!
//! - `config`  ：端末ローカルの設定と OS キーチェーン
//! - `store`   ：暗号文の置き場所（同期フォルダ）
//! - `drive`   ：暗号文の置き場所（Google Drive API、OAuth＋PKCE）
//! - `session` ：解除・保存・マージ・差し戻し検知などの操作
//! - `kit`     ：Emergency Kit
//! - `bridge`  ：ブラウザ拡張との連絡路（Native Messaging）
//! - `import`  ：他のツールが書き出した CSV の取り込み
//!
//! 画面への表示は行わない。注意メッセージは `Session::notices` などで呼び出し側に返す。
#![forbid(unsafe_code)]

pub mod bridge;
pub mod config;
pub mod drive;
pub mod import;
pub mod kit;
pub mod links;
pub mod session;
pub mod store;

pub use session::*;

pub type R<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
