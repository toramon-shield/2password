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
FILL="{\"type\":\"fill\",\"origin\":\"https://shop.example\",\"id\":\"$SHOP\"}"
check "拡張：初めてのサイトでは、アプリでの確認なしにパスワードを返さない" 'O=$(br "$FILL"); echo "$O" | grep -q "needs_approval" && ! echo "$O" | grep -q "P@ss"'
check "拡張：アプリで許可するとパスワードを返す" 'printf "%s\n" "$PW2" | A bridge-request --approve "$FILL" | grep -q "\"username\":\"carol\""'
check "拡張：許可済みのサイトには次回から確認なしで返す" 'br "$FILL" | grep -q "\"username\":\"carol\""'
check "拡張：許可の記録にサイト名を平文で残さない" '[ -f "$WORK/devA/approved-sites.json" ] && ! grep -q "shop" "$WORK/devA/approved-sites.json"'
check "拡張：同じドメインでも別のホストにはパスワードを返さない" 'br "{\"type\":\"fill\",\"origin\":\"https://blog.shop.example\",\"id\":\"$SHOP\"}" | grep -q "no_match"'
check "拡張：同じドメインの別ホストでは、候補を出さずに知らせる" 'O=$(br "{\"type\":\"list\",\"origin\":\"https://blog.shop.example\"}"); echo "$O" | grep -q "\"items\":\[\]" && echo "$O" | grep -q "same_site"'
check "拡張：似た名前の別サイトでは注意を出す" 'br "{\"type\":\"list\",\"origin\":\"https://shop.example.evil.test\"}" | grep -q "lookalike"'
check "拡張：無関係のサイトでは保存済みのサイト名を教えない" 'O=$(br "{\"type\":\"list\",\"origin\":\"https://unrelated.test\"}"); ! echo "$O" | grep -q "related" && ! echo "$O" | grep -q "shop"'
check "拡張：偽サイトにはパスワードを返さない" 'O=$(br "{\"type\":\"fill\",\"origin\":\"https://shop.example.evil.test\",\"id\":\"$SHOP\"}"); echo "$O" | grep -q "no_match" && ! echo "$O" | grep -q "P@ss"'
check "拡張：http のページにはパスワードを返さない" 'O=$(br "{\"type\":\"fill\",\"origin\":\"http://shop.example\",\"id\":\"$SHOP\"}"); echo "$O" | grep -q "insecure" && ! echo "$O" | grep -q "P@ss"'
check "拡張：http のページでは保存もしない" 'br "{\"type\":\"save\",\"origin\":\"http://plain.example\",\"username\":\"x\",\"password\":\"y\"}" | grep -q "insecure"'
check "拡張：保存済みのログインは保存を聞かない" 'br "{\"type\":\"check\",\"origin\":\"https://shop.example\",\"username\":\"carol\",\"password\":\"Sh,op-\\\"P@ss\"}" | grep -q "\"result\":\"known\""'
check "拡張：パスワードが変わっていたら更新を提案する" 'br "{\"type\":\"check\",\"origin\":\"https://shop.example\",\"username\":\"carol\",\"password\":\"new-pass\"}" | grep -q "\"result\":\"update\""'
br "{\"type\":\"update\",\"origin\":\"https://shop.example\",\"id\":\"$SHOP\",\"password\":\"new-pass\"}" > /dev/null
check "拡張：パスワードの更新が他の端末にも届く" 'printf "%s\n" "$PW2" | Bd show Shop --reveal | grep -q "new-pass"'
check "拡張：別サイトからは既存の項目を更新できない" 'br "{\"type\":\"update\",\"origin\":\"https://evil.test\",\"id\":\"$SHOP\",\"password\":\"hacked\"}" | grep -q "no_match"'
br '{"type":"save","origin":"https://news.example","username":"dave","password":"News-Pass-1"}' > /dev/null
NEWS=$(br '{"type":"list","origin":"https://news.example"}' | grep -o '"id":"[0-9a-f-]*"' | head -1 | cut -d'"' -f4)
check "拡張：ブラウザで保存した項目は、そのサイトで確認なしに使える" 'br "{\"type\":\"fill\",\"origin\":\"https://news.example\",\"id\":\"$NEWS\"}" | grep -q "News-Pass-1"'
check "拡張：新しいログインを保存できる" 'O=$(printf "%s\n" "$PW2" | Bd show news.example --reveal); echo "$O" | grep -q "News-Pass-1" && echo "$O" | grep -q "https://news.example"'

# パスキー（作成と署名はアプリの中で行い、拡張には結果だけを返す）
OUT=$(br '{"type":"passkey_create","origin":"https://login.shop.example","rp_id":"shop.example","rp_name":"Shop Passkey","challenge":"Y2hhbGxlbmdl","user_id":"dXNlci0x","user_name":"erin"}')
check "パスキー：作成結果に秘密鍵を含めない" 'echo "$OUT" | grep -q "attestation_object" && ! echo "$OUT" | grep -q -i -E "private|pkcs8"'
PKID=$(br '{"type":"passkey_list","origin":"https://shop.example","rp_id":"shop.example"}' | grep -o '"id":"[0-9a-f]*"' | head -1 | cut -d'"' -f4)
check "パスキー：一覧に鍵を含めない" '[ -n "$PKID" ] && ! br "{\"type\":\"passkey_list\",\"origin\":\"https://shop.example\",\"rp_id\":\"shop.example\"}" | grep -q -i -E "private|credential"'
pkget() { br "{\"type\":\"passkey_get\",\"origin\":\"$1\",\"rp_id\":\"$2\",\"id\":\"$PKID\",\"challenge\":\"bG9naW4\"}"; }
check "パスキー：作ったページでは署名を返す" 'pkget https://login.shop.example shop.example | grep -q "\"signature\""'
check "パスキー：同じサイトでも初めてのホストでは、アプリの確認なしに署名しない" 'O=$(pkget https://shop.example shop.example); echo "$O" | grep -q "needs_approval" && ! echo "$O" | grep -q "signature"'
check "パスキー：偽サイトが本物のサイト名を名乗っても署名しない" 'O=$(pkget https://shop.example.evil.test shop.example); echo "$O" | grep -q "no_match" && ! echo "$O" | grep -q "signature"'
check "パスキー：偽サイトが自分のサイト名で求めても署名しない" 'pkget https://shop.example.evil.test shop.example.evil.test | grep -q "no_match"'
check "パスキー：http のページでは使えない" 'pkget http://shop.example shop.example | grep -q "insecure"'
check "パスキー：パスキーだけの項目は、パスワードの候補に出さない" '! br "{\"type\":\"list\",\"origin\":\"https://login.shop.example\"}" | grep -q "Shop Passkey"'
check "パスキー：他の端末にも同期される" 'printf "%s\n" "$PW2" | Bd bridge-request "{\"type\":\"passkey_list\",\"origin\":\"https://shop.example\",\"rp_id\":\"shop.example\"}" | grep -q "erin"'
check "パスキー：保存先のファイルにサイト名が平文で現れない" '! grep -r -q -a -E "shop\.example|erin" "$WORK/drive"'

# ワンタイムパスワード（2 段階認証）。設定キーは保管庫の中に置き、拡張にはその時刻のコードだけを返す
printf '%s\n%s\n' "$PW2" 'jbsw y3dp ehpk 3pxp' | A totp "$SHOP" --set > /dev/null
check "2FA：登録した項目から 6 桁のコードが出る" 'printf "%s\n" "$PW2" | A totp "$SHOP" | grep -q -E "^[0-9]{6}（あと [0-9]+ 秒）"'
check "2FA：一致するサイトにはコードを返し、設定キーは返さない" 'O=$(br "{\"type\":\"totp\",\"origin\":\"https://shop.example\",\"id\":\"$SHOP\"}"); echo "$O" | grep -q -E "\"code\":\"[0-9]{6}\"" && ! echo "$O" | grep -q -i -E "JBSW|otpauth|secret"'
check "2FA：偽サイトにはコードを返さない" 'O=$(br "{\"type\":\"totp\",\"origin\":\"https://shop.example.evil.test\",\"id\":\"$SHOP\"}"); echo "$O" | grep -q "no_match" && ! echo "$O" | grep -q "\"code\""'
check "2FA：コードのない項目には返さない" 'br "{\"type\":\"totp\",\"origin\":\"https://news.example\",\"id\":\"$NEWS\"}" | grep -q "no_match"'
check "2FA：一覧では、コードを持つ項目かどうかだけを知らせる" 'br "{\"type\":\"list\",\"origin\":\"https://shop.example\"}" | grep -q "\"totp\":true"'
check "2FA：他の端末でもコードが出る" 'printf "%s\n" "$PW2" | Bd totp "$SHOP" | grep -q -E "^[0-9]{6}"'
printf 'Title,Url,Username,Password,OTPAuth,Notes\nMail2FA,https://mail.example,frank,Mail-Pass-1,otpauth://totp/Mail:frank?secret=JBSWY3DPEHPK3PXP&issuer=Mail,\n' > "$WORK/export2.csv"
printf '%s\n' "$PW2" | A import "$WORK/export2.csv" > /dev/null
check "2FA：1Password 形式の CSV から設定を取り込める" 'printf "%s\n" "$PW2" | A totp Mail2FA | grep -q -E "^[0-9]{6}"'
printf '%s\n' "$PW2" | A totp Mail2FA --clear > /dev/null
check "2FA：登録を外すとコードは出ない" '! (printf "%s\n" "$PW2" | A totp Mail2FA > /dev/null 2>&1)'
check "2FA：保存先のファイルに設定キーが平文で現れない" '! grep -r -q -a -i -E "JBSWY3DP|otpauth" "$WORK/drive"'

A forget > /dev/null; Bd forget > /dev/null
echo "結果: 成功 $pass / 失敗 $fail"
[ "$fail" -eq 0 ]
