# Google Cloud で OAuth クライアントを作る

> 所要時間：約15分
> 前提：Google アカウント（個人の Gmail で可）、ブラウザ
> 確認日：2026-11-11（Google Cloud コンソールは画面名が変わることがあります）

## この章のゴール

- 2password が **自分の Google Drive の、自分が作ったファイルだけ** にアクセスできる「鍵穴」を用意する。
- そのために、Google Cloud で次の3つを作る。
  1. プロジェクト（入れ物）
  2. OAuth 同意画面（「このアプリに Drive の権限を渡しますか？」という確認画面の設定）
  3. OAuth クライアント ID（デスクトップアプリ用）

## なぜこれが必要か

1Password は、ログインに **SRP** という仕組みを使います（[1Password の解説](https://support.1password.com/secure-remote-password/)）。2password には自前のサーバーがないので、Google Drive への出入りは **Google の OAuth** に任せます。

ここで役割をもう一度整理します。

| 守るもの | 担当 |
|---|---|
| 中身を読まれない（機密性） | 端末内の暗号化（パスワード＋Secret Key） |
| Drive に出入りできる人を限る（アクセス制御） | Google アカウント＋OAuth |
| ファイルが消えても戻せる（可用性） | 端末の控え＋Emergency Kit |

OAuth を設定しても、**Google にも、このアプリの作者にも、保管庫の中身は読めません**。OAuth が決めるのは「どのファイルに触れてよいか」だけです。

### 権限は `drive.file` だけにする

| スコープ | 触れる範囲 | 2password での採用 |
|---|---|---|
| `drive` | Drive のすべてのファイル | ✕ 広すぎる |
| `drive.readonly` | すべてのファイルを読める | ✕ |
| `drive.appdata` | アプリ専用の隠しフォルダ | △ 利用者が暗号文を目で確認できない |
| **`drive.file`** | **このアプリが作った・開いたファイルだけ** | **◎ 採用** |

万一このアプリのトークンが盗まれても、Drive にある他の書類（請求書、契約書など）には触れません。これが「最小権限」の考え方です。

---

## 手順

### 手順1：プロジェクトを作る

1. https://console.cloud.google.com/ を開き、Google アカウントでログインする。
2. 画面上部の **プロジェクト選択** → **新しいプロジェクト**。
3. プロジェクト名：`2password`（任意）。組織がなければ「組織なし」のままでよい。
4. **作成** を押し、作成後に上部のプロジェクト選択が `2password` になっていることを確認する。

### 手順2：Google Drive API を有効にする

1. 左メニュー（☰）→ **API とサービス** → **ライブラリ**。
2. 「Google Drive API」を検索して開き、**有効にする** を押す。

> よくあるつまずき：これを忘れると、後でアプリから呼んだときに `403 accessNotConfigured` になります。

### 手順3：OAuth 同意画面（Google Auth Platform）を設定する

左メニュー → **API とサービス** → **OAuth 同意画面**（または **Google Auth Platform**）を開き、**開始** を押します。

1. **アプリ情報**
   - アプリ名：`2password`
   - ユーザー サポートメール：自分のメールアドレス
2. **対象（Audience）**：**外部（External）** を選ぶ。
   - 個人の Gmail では「内部」は選べません（Google Workspace の組織向けです）。
3. **連絡先情報**：自分のメールアドレス。
4. ポリシーに同意して **作成**。

続けて、左の各項目を設定します。

- **対象（Audience）** → **テストユーザー** → **ユーザーを追加** → 自分の Gmail アドレスを追加。
  - 公開ステータスが「テスト中」の間は、ここに登録した人しかログインできません。
- **データアクセス（Data Access）** → **スコープを追加または削除** → 次の1つだけにチェックして保存。
  - `https://www.googleapis.com/auth/drive.file`
  - （「このアプリで使用する Google ドライブのファイルのみ参照、編集、作成、削除」と表示されるもの）

### 手順4：OAuth クライアント ID（デスクトップアプリ）を作る

1. 左の **クライアント（Clients）** → **クライアントを作成**。
2. アプリケーションの種類：**デスクトップ アプリ**。
3. 名前：`2password CLI`。
4. **作成** → 表示されるダイアログで **JSON をダウンロード**。

ダウンロードしたファイル（`client_secret_xxxx.json`）を、次の場所に `oauth-client.json` という名前で置きます。

| OS | 置き場所 |
|---|---|
| Windows | `%APPDATA%\2password\oauth-client.json` |
| macOS | `~/Library/Application Support/2password/oauth-client.json` |
| Linux | `~/.config/2password/oauth-client.json` |

> ⚠ このファイルは **Git にコミットしない・Drive に置かない・チャットに貼らない**。
> デスクトップアプリの「クライアント シークレット」は、Google の説明上も配布物に含まれる前提で、完全な秘密としては扱われません。だからこそ、2password は **PKCE**（毎回使い捨ての合言葉を作る仕組み）を必ず併用します。それでも、むやみに公開しないのが基本です。

### 手順5（任意・推奨）：「テスト中」のまま使うときの注意

公開ステータスが **テスト中** で、外部ユーザー向けのアプリの場合、**リフレッシュトークンが7日で失効** します。つまり、7日ごとにブラウザでのログインをやり直すことになります。

- 学習・試用中：テスト中のままで問題ありません。
- 日常的に使う：**対象 → アプリを公開** で「本番環境」にします。`drive.file` は Google の分類で機密性の高いスコープではないため、通常は Google の審査（検証）なしで公開できます。ただし公開前に、その時点の Google の案内（検証が必要な条件）を必ず確認してください。

### 手順6：Google アカウント自体を守る

OAuth の入口は Google アカウントです。中身は暗号で守られていても、アカウントを乗っ取られるとファイルの削除や古い版への差し戻しはされ得ます（2password は差し戻しを検知し、端末の控えから復元します）。

- Google アカウントに **パスキー** または **2段階認証** を設定する。
- https://myaccount.google.com/permissions で、許可したアプリを定期的に確認する（2password が `drive.file` だけを持っていることを確認できる）。

---

## 確認チェックリスト

- [ ] プロジェクト `2password` が選択されている
- [ ] Google Drive API が「有効」
- [ ] 同意画面：外部、テストユーザーに自分を追加済み
- [ ] スコープは `drive.file` の1つだけ
- [ ] デスクトップ アプリのクライアントを作成し、`oauth-client.json` を所定の場所に置いた
- [ ] `oauth-client.json` を Git・Drive・チャットに置いていない

## 確認クイズ

1. 2password が Drive API に要求するスコープはどれか。
   a. `drive`　b. `drive.readonly`　**c. `drive.file`**　d. `drive.appdata`
   → 解説：アプリが作ったファイルだけに触れる最小権限だから。
2. OAuth を設定すると、Google は保管庫の中身を読めるようになるか。
   → **読めない。** Drive に送られるのは端末で暗号化した `.2pw` ファイルだけで、OAuth は「どのファイルに触れてよいか」を決めるだけ。
3. 公開ステータスが「テスト中」のとき、何日ごとに再ログインが必要になるか。
   → **7日。** 日常利用では本番環境に公開する。
4. デスクトップアプリのクライアント シークレットだけに頼らず、2password が併用する仕組みは何か。
   → **PKCE**（認可のたびに使い捨ての code_verifier を作り、横取りされた認可コードを使えなくする）。

## 次の章

[03-drive-api.md](03-drive-api.md) では、ここで作った `oauth-client.json` を使って、`2pw` の保存先を「同期フォルダ」から「Drive API」に切り替えます。ブラウザでのログイン（ループバック＋PKCE）、トークンの OS キーチェーンへの保存、競合検知（`headRevisionId`）を実装します。

## 参考

- Google Drive API のスコープの選び方：https://developers.google.com/workspace/drive/api/guides/api-specific-auth
- ネイティブアプリの OAuth（RFC 8252）、PKCE（RFC 7636）
- OAuth 同意画面とテストユーザー：https://support.google.com/cloud/answer/15549945
