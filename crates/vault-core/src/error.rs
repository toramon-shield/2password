use thiserror::Error;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    /// パスワード違い・Secret Key 違い・改ざんを外部に区別して返さない（推測の手がかりを与えない）。
    #[error(
        "解除できませんでした（パスワードまたは Secret Key が違うか、ファイルが改ざんされています）"
    )]
    Unlock,
    #[error("ファイル形式が正しくありません: {0}")]
    Format(&'static str),
    #[error("鍵導出パラメータが安全な範囲外です（攻撃者による弱体化の可能性）")]
    WeakKdf,
    #[error("Secret Key の形式が正しくありません")]
    SecretKeyFormat,
    #[error("Secret Key が別のアカウントのものです")]
    AccountMismatch,
    #[error(
        "古い版の保管庫を受け取りました（番号 {got} < 既知の {known}）。差し戻し攻撃の可能性があるため拒否しました"
    )]
    Rollback { got: u64, known: u64 },
    #[error("OS の乱数生成に失敗しました")]
    Rng,
    #[error("内部エラー: {0}")]
    Internal(&'static str),
}
