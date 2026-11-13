#!/usr/bin/env bash
# 2台の端末をフォルダで模擬して、2pw の一連の動作を確かめる。
# 使い方: scripts/e2e.sh [作業フォルダ]   ※実際の Google Drive には触れない
set -u
WORK="${1:-$(mktemp -d)}"
rm -rf "$WORK" && mkdir -p "$WORK"
BIN="${BIN:-./target/release/2pw}"
PW='blue whale coffee mountain 2026'
PW2='green river piano autumn 2027'
A()  { TWOPW_HOME="$WORK/devA" "$BIN" --password-stdin "$@"; }
Bd() { TWOPW_HOME="$WORK/devB" "$BIN" --password-stdin "$@"; }
pass=0; fail=0
check() { if eval "$2"; then echo "  ✓ $1"; pass=$((pass+1)); else echo "  ✕ $1"; fail=$((fail+1)); fi; }

printf '%s\n%s\n' "$PW" "$PW" | A init --store "$WORK/drive" --kit "$WORK/kit.html" > "$WORK/init.log"
SK=$(grep -o '2P1-[A-Z0-9-]*' "$WORK/init.log" | head -1)
check "init で Secret Key と Emergency Kit ができる" '[ -n "$SK" ] && [ -f "$WORK/kit.html" ]'

printf '%s\n%s\n' "$PW" 'Bank-P@ss-123' | A add "○○銀行" --url https://bank.example -u alice > /dev/null
printf '%s\n%s\n' "$SK" "$PW" | Bd join --store "$WORK/drive" > /dev/null
check "2台目が Secret Key とパスワードで参加できる" '[ -f "$WORK/devB/config.json" ]'

printf '%s\n' "$PW" | Bd add "AWS root" -u admin --generate > /dev/null
check "2台目の追加が1台目から見える" 'printf "%s\n" "$PW" | A list | grep -q "AWS root"'
check "パスワードを復号して表示できる" 'printf "%s\n" "$PW" | A show 銀行 --reveal | grep -q "Bank-P@ss-123"'
check "Drive 上のファイルに平文がない" '! grep -q -E "銀行|bank|AWS|alice|Bank-P" "$WORK"/drive/*'
check "間違ったパスワードは拒否" '! (printf "wrong\n" | A list > /dev/null 2>&1)'

V=$(ls "$WORK"/drive/vault-*.2pw); cp "$V" "$WORK/good.2pw"
python -I -c "import sys;p=sys.argv[1];b=bytearray(open(p,'rb').read());b[-100]^=1;open(p,'wb').write(b)" "$V"
check "1バイトの改ざんを検知" '! (printf "%s\n" "$PW" | A list > /dev/null 2>&1)'
cp "$WORK/good.2pw" "$V"

cp "$V" "$WORK/old.2pw"
printf '%s\n' "$PW" | A add "Cloudflare" -g > /dev/null
cp "$WORK/old.2pw" "$V"
OUT=$(printf '%s\n' "$PW" | A list 2>&1)
check "差し戻しを検知し、最新版で復元する" 'echo "$OUT" | grep -q "差し戻され" && echo "$OUT" | grep -q Cloudflare'
check "復元後の保管場所が最新版になる" 'printf "%s\n" "$PW" | Bd list | grep -q Cloudflare'

rm "$V"
printf '%s\n' "$PW" | A list > /dev/null 2>&1
check "保管庫ファイルを消されても控えから書き戻す" '[ -f "$V" ]'

# 保管場所への書き込み失敗（通信エラーの代わりに書き込み禁止で再現）
block() { if command -v cygpath > /dev/null; then attrib +r "$(cygpath -w "$V")"; else chmod a-w "$WORK/drive"; fi; }
unblock() { if command -v cygpath > /dev/null; then attrib -r "$(cygpath -w "$V")"; else chmod u+w "$WORK/drive"; fi; }
block
OUT=$(printf '%s\n' "$PW" | A add "Offline item" -g 2>&1)
unblock
check "保存失敗時は端末に保存済みで後で書き戻すと案内する" 'echo "$OUT" | grep -q "次の保存・同期・解除のときに書き戻します"'
OUT=$(printf '%s\n' "$PW" | A list 2>&1)
check "保存が届かなかっただけなら改ざん警告を出さない" 'echo "$OUT" | grep -q "届いていません" && ! echo "$OUT" | grep -q "改ざん"'
check "届かなかった変更が書き戻され、2台目から見える" 'printf "%s\n" "$PW" | Bd list | grep -q "Offline item"'

# 同期ツールが作る競合コピー：マージ後に .merged へ改名して片付ける
cp "$WORK/old.2pw" "${V%.2pw} (1).2pw"
OUT=$(printf '%s\n' "$PW" | A list 2>&1)
check "競合コピーをマージして片付ける" 'echo "$OUT" | grep -q "片付けました" && [ ! -f "${V%.2pw} (1).2pw" ] && [ -f "${V%.2pw} (1).2pw.merged" ]'

cp "$WORK/drive/account.2pw" "$WORK/old-account.2pw"
printf '%s\n%s\n%s\n' "$PW" "$PW2" "$PW2" | A passwd > /dev/null
check "パスワード変更には現在のパスワードが必要" '! (printf "%s\n%s\n%s\n" "wrong" "$PW" "$PW" | A passwd > /dev/null 2>&1)'
check "パスワード変更後、2台目は旧パスワードで開けない" '! (printf "%s\n" "$PW" | Bd list > /dev/null 2>&1)'
check "パスワード変更後、2台目は新パスワードで開ける" 'printf "%s\n" "$PW2" | Bd list | grep -q "○○銀行"'

cp "$WORK/old-account.2pw" "$WORK/drive/account.2pw"
check "変更前の account.2pw を書き戻されても旧パスワードでは開けない" '! (printf "%s\n" "$PW" | A list > /dev/null 2>&1)'
OUT=$(printf '%s\n' "$PW2" | A list 2>&1)
check "書き戻しを検知し、控えの新しい account.2pw で復元する" 'echo "$OUT" | grep -q "差し戻され" && ! cmp -s "$WORK/old-account.2pw" "$WORK/drive/account.2pw"'

printf '%s\n' "$PW2" | A rm Cloudflare > /dev/null
check "削除が他の端末にも反映される（墓石）" '! (printf "%s\n" "$PW2" | Bd list | grep -q Cloudflare)'

# CSV の取り込み（1Password の書き出し形式）
printf 'Title,Url,Username,Password,OTPAuth,Favorite,Archived,Tags,Notes\nShop,https://shop.example/login,carol,"Sh,op-""P@ss",,false,false,,memo\n' > "$WORK/export.csv"
OUT=$(printf '%s\n' "$PW2" | A import "$WORK/export.csv")
check "1Password 形式の CSV を取り込める" 'echo "$OUT" | grep -q "1 件を取り込みました" && printf "%s\n" "$PW2" | A show Shop --reveal | grep -q "Sh,op-\"P@ss"'
OUT=$(printf '%s\n' "$PW2" | A import "$WORK/export.csv")
check "同じ CSV をもう一度取り込んでも重複しない" 'echo "$OUT" | grep -q "0 件を取り込みました"'

# ブラウザ拡張からの要求（アプリと同じ処理を CLI から呼ぶ）
br() { printf '%s\n' "$PW2" | A bridge-request "$1"; }
check "拡張：一致するサイトの項目だけを一覧に出す（パスワードは含めない）" 'O=$(br "{\"type\":\"list\",\"origin\":\"https://www.shop.example\"}"); echo "$O" | grep -q "\"title\":\"Shop\"" && ! echo "$O" | grep -q -E "P@ss|銀行"'
SHOP=$(br '{"type":"list","origin":"https://shop.example"}' | grep -o '"id":"[0-9a-f]*"' | head -1 | cut -d'"' -f4)
check "拡張：一致するサイトにはパスワードを返す" 'br "{\"type\":\"fill\",\"origin\":\"https://shop.example\",\"id\":\"$SHOP\"}" | grep -q "\"username\":\"carol\""'
check "拡張：偽サイトにはパスワードを返さない" 'O=$(br "{\"type\":\"fill\",\"origin\":\"https://shop.example.evil.test\",\"id\":\"$SHOP\"}"); echo "$O" | grep -q "no_match" && ! echo "$O" | grep -q "P@ss"'
check "拡張：http のページにはパスワードを返さない" 'br "{\"type\":\"fill\",\"origin\":\"http://shop.example\",\"id\":\"$SHOP\"}" | grep -q "no_match"'
check "拡張：保存済みのログインは保存を聞かない" 'br "{\"type\":\"check\",\"origin\":\"https://shop.example\",\"username\":\"carol\",\"password\":\"Sh,op-\\\"P@ss\"}" | grep -q "\"result\":\"known\""'
check "拡張：パスワードが変わっていたら更新を提案する" 'br "{\"type\":\"check\",\"origin\":\"https://shop.example\",\"username\":\"carol\",\"password\":\"new-pass\"}" | grep -q "\"result\":\"update\""'
br "{\"type\":\"update\",\"origin\":\"https://shop.example\",\"id\":\"$SHOP\",\"password\":\"new-pass\"}" > /dev/null
check "拡張：パスワードの更新が他の端末にも届く" 'printf "%s\n" "$PW2" | Bd show Shop --reveal | grep -q "new-pass"'
check "拡張：別サイトからは既存の項目を更新できない" 'br "{\"type\":\"update\",\"origin\":\"https://evil.test\",\"id\":\"$SHOP\",\"password\":\"hacked\"}" | grep -q "no_match"'
br '{"type":"save","origin":"https://news.example","username":"dave","password":"News-Pass-1"}' > /dev/null
check "拡張：新しいログインを保存できる" 'O=$(printf "%s\n" "$PW2" | Bd show news.example --reveal); echo "$O" | grep -q "News-Pass-1" && echo "$O" | grep -q "https://news.example"'

A forget > /dev/null; Bd forget > /dev/null
echo "結果: 成功 $pass / 失敗 $fail"
[ "$fail" -eq 0 ]
