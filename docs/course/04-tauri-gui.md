# Tauri 2 でデスクトップアプリにし、インストーラーを作る

> 所要時間：約30分
> 前提：[README](../../README.md) のチュートリアルと [03-drive-api.md](03-drive-api.md) の CLI が動いていること
> 確認日：2026-11-11（Windows 11 で GUI の作成〜解除〜追加〜ロックまで自動操作で確認済み。macOS は GitHub Actions でビルド）

## この章のゴール

- CLI と**同じ暗号コード**を使ったデスクトップアプリ（Windows / macOS）を作る。
- プログラミングを知らない人でも、**ダブルクリックでインストール**できる形にする。
- GUI ならではの危険（画面にパスワードを出す、離席中に開きっぱなし）に対策する。

## なぜ Tauri なのか

| 候補 | 特徴 | 2password での評価 |
|---|---|---|
| Electron | Chromium 同梱、JavaScript 中心 | アプリが 100MB 超。暗号を JS で書くことになりがち |
| ネイティブ（WinUI / SwiftUI） | OS ごとに別の言語で書く | 2つ作る必要がある |
| **Tauri 2** | 画面は OS 標準の WebView、処理は **Rust** | **約10MB**。CLI の Rust コードをそのまま使える。将来 iOS / Android にも対応 |

## 設計：CLI と GUI でコードを共有する

```
crates/vault-core     暗号とデータ形式（I/O なし）
crates/vault-client   設定・保管場所・Drive・セッション   ← CLI と GUI が共有
crates/vault-cli      2pw コマンド（薄い層）
app/src-tauri         デスクトップアプリ（薄い層）
app/ui                画面（HTML / CSS / JS。ビルド不要）
```

## GUI で追加した安全対策

| 対策 | 実装 | 防ぐもの |
|---|---|---|
| パスワードは「表示」を押したときだけ画面に渡す | 一覧・詳細のデータにパスワードを含めない（`reveal_password` だけが返す） | 画面側（WebView）からの漏えい |
| コピーは Rust 側で行い、30秒後に消す | `copy_field` → arboard。Windows はクリップボード履歴・クラウド同期から除外 | クリップボードへの置き忘れ |
| 表示したパスワードは20秒で隠す | 画面側のタイマー | 画面ののぞき見 |
| 無操作5分で自動ロック | Rust 側の監視スレッドが Session を破棄（鍵は zeroize） | 離席中の不正利用 |
| 厳格な CSP | 外部スクリプト・インライン スクリプト・外部通信を禁止 | XSS・外部への送信 |
| 権限（capabilities）は最小 | `core:default` と自作コマンドだけ。ファイル操作・シェル実行のプラグインは入れない | 画面側からの OS 操作 |
| 外部リンクは http(s) と、この起動中に作った Emergency Kit だけ開く | `open_url`（URL を解析し、http/https・ホスト名あり・認証情報なしだけ許可）／`open_kit`（画面からパスを受け取らない） | 任意のファイル・UNC パス・独自スキームを開かせる攻撃 |
| 重い処理は UI スレッドの外 | `spawn_blocking`（Argon2id・通信・ブラウザ待ち） | 画面の固まり |
| Google ログインは5分で打ち切り | ループバックの待ち受けにタイムアウト | ブラウザを閉じたときの固まり |

## インストーラー

| OS | 形式 | 作り方 |
|---|---|---|
| Windows | `.exe`（NSIS、ユーザー単位でインストール、管理者権限不要） | `npx @tauri-apps/cli@2.12.4 build --bundles nsis`（Windows 上） |
| macOS | `.dmg`（Apple Silicon / Intel 両対応の universal） | GitHub Actions（`.github/workflows/installers.yml`） |

macOS 用は Mac 上でしかビルドできないため、GitHub Actions の macOS 環境でビルドします。

### Google ログインの設定をアプリに埋め込む

一般の利用者に `oauth-client.json` を置いてもらうのは現実的ではありません。そこで、ビルド時に埋め込めるようにしました。

```powershell
$env:TWOPW_EMBED_OAUTH_CLIENT = "$env:APPDATA\2password\oauth-client.json"
npx @tauri-apps/cli@2.12.4 build --bundles nsis
```

GitHub Actions では、リポジトリの Secret `OAUTH_CLIENT_JSON` に JSON の中身を登録します。

> ⚠ デスクトップアプリのクライアント シークレットは、Google の説明上も「配布物に含まれ、秘密にできない値」です。だから PKCE を必ず併用しています。ただし、インストーラーを一般公開すると、他人があなたの OAuth クライアント（アプリ名・利用枠）を流用できます。公開配布する場合は、配布用に別の OAuth クライアントを作り、同意画面を「本番環境」に公開してください（テスト中はテストユーザーしかログインできず、7日でログインが切れます）。

### 署名について

- Windows：コード署名証明書がないと SmartScreen の警告が出る（「詳細情報 → 実行」で起動）。
- macOS：Apple Developer Program（有料）で署名・公証しないと「開発元を検証できません」と出る（右クリック → 開く）。
- 学習・身内での利用なら署名なしで十分。広く配布するなら署名は必須。

## ハンズオン

1. `cargo build --release -p twopassword-app` でアプリ本体をビルドし、`target\release\2password.exe` を起動する。
2. **新しく始める** → 保存先 → パスワード → Secret Key 表示 → Emergency Kit 印刷。
3. 項目を追加し、**コピー**して30秒後にクリップボードが空になることを確かめる。
4. 🔒 でロックし、間違ったパスワードと正しいパスワードを試す。
5. `npx @tauri-apps/cli@2.12.4 build --bundles nsis` でインストーラーを作り、別の Windows アカウントでインストールしてみる。

## よくあるつまずき

| 症状 | 原因と対処 |
|---|---|
| `dlltool.exe: program not found` | GNU ツールチェーンで MinGW が入っていない。`winget install BrechtSanders.WinLibs.POSIX.UCRT --scope user`。または Visual Studio Build Tools を入れて MSVC でビルド |
| 起動すると真っ白 | CSP 違反（インライン スクリプトなど）。開発者ツールのコンソールを確認 |
| `ロックされています` | 自動ロック後に操作した。もう一度解除する |
| Mac で「壊れているため開けません」 | ダウンロード時の隔離属性。`xattr -dr com.apple.quarantine /Applications/2password.app` |

## 確認クイズ

1. 一覧画面のデータにパスワードを含めないのはなぜか。
   → 画面側（WebView）は攻撃面が広い。必要な瞬間だけ渡すことで、漏えいの機会を減らすため。
2. 自動ロックで「鍵を消す」とは、具体的に何をしているか。
   → Rust 側の `Session` を破棄し、Vault Key・平文を zeroize（0 で上書き）している。画面を隠すだけではない。
3. インストーラーに OAuth クライアントを埋め込んでも、保管庫の安全性が下がらないのはなぜか。
   → OAuth は「Drive のどのファイルに触れてよいか」を決めるだけ。中身はパスワード＋Secret Key で暗号化されており、OAuth の情報では復号できない。

## 次の一歩

- iOS / Android 版（Tauri 2 のモバイル対応、生体認証プラグイン）
- コード署名と自動アップデート
- 第三者によるセキュリティレビュー
