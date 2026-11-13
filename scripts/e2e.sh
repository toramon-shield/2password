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

A forget > /dev/null; Bd forget > /dev/null
echo "結果: 成功 $pass / 失敗 $fail"
[ "$fail" -eq 0 ]
