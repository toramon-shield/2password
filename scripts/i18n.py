#!/usr/bin/env python3
"""UI の翻訳ファイルの検査と、Rust のメッセージの抜き出し。

使い方:
    python scripts/i18n.py check            # すべての言語の翻訳ファイルを検査する
    python scripts/i18n.py check fr de      # 指定した言語だけを検査する
    python scripts/i18n.py rust             # 画面とコマンドラインに出る Rust のメッセージを一覧する
    python scripts/i18n.py sync-rust        # ja.json の rust 節を、いまの Rust のソースに合わせる

翻訳ファイル:
    app/ui/locales/<言語>.json              デスクトップアプリの画面
        "ui":   キー → 文言。{name} は差し込む値
        "rust": Rust が返す日本語のメッセージ（原文）→ 訳。{} は差し込む値。
                コマンドライン（2pw）と、Google ログインの完了ページも同じ訳を使う
    extension/_locales/<言語>/messages.json ブラウザ拡張（Chrome の形式。$1 は差し込む値）

ja.json と extension/_locales/ja が原本。ほかの言語は、原本と同じキーと差し込み口を持つこと。
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
APP = ROOT / "app" / "ui" / "locales"
EXT = ROOT / "extension" / "_locales"

# アプリの言語コード → Chrome の言語フォルダ名
LANGS = {
    "ja": ["ja"], "en": ["en"], "zh": ["zh_CN"], "pt": ["pt_BR", "pt_PT"], "es": ["es"],
    "fr": ["fr"], "it": ["it"], "vi": ["vi"], "th": ["th"], "ms": ["ms"], "ru": ["ru"],
    "de": ["de"], "hi": ["hi"], "ar": ["ar"], "tr": ["tr"],
}

# 画面とコマンドラインに出る Rust のメッセージを探すファイル（テストとコメントは除く）
RUST_FILES = sorted((ROOT / "crates" / "vault-client" / "src").glob("*.rs")) + [
    ROOT / "crates" / "vault-core" / "src" / "error.rs",
    ROOT / "app" / "src-tauri" / "src" / "main.rs",
    ROOT / "crates" / "vault-cli" / "src" / "main.rs",
    ROOT / "crates" / "vault-cli" / "src" / "attack.rs",
]
# コマンドの説明（clap は /// のコメントを --help に出す）。この印より前の /// を集める
CLI_HELP = (ROOT / "crates" / "vault-cli" / "src" / "main.rs", "// ---------------- 入力")
# ブラウザ拡張にだけ返すもの（拡張は error のコードで訳す）と、ファイルの中身を作るもの
RUST_SKIP = {"bridge.rs", "kit.rs"}
# メッセージではない文字列（CSV の列名、フォルダ名、ブラウザに出す HTML の枠）
RUST_NOT_MESSAGES = {"タイトル", "名前", "ユーザー名", "パスワード", "メモ", "マイドライブ", "{d}:\マイドライブ"}

JA = re.compile("[぀-ヿ一-鿿]")
LIT = re.compile(r'"((?:[^"\\]|\\.)*)"')
APP_SLOT = re.compile(r"\{[a-zA-Z_][a-zA-Z0-9_]*\}")
RUST_SLOT = re.compile(r"\{[^{}]*\}")
EXT_SLOT = re.compile(r"\$[1-9]")
TAG = re.compile(r"</?[a-z]+>")
# 改行とタブ以外の制御文字（置換の失敗で混ざると、画面に見えない文字として出る）
CONTROL = re.compile(r"[\x00-\x08\x0b-\x1f\x7f]")


def unescape(s: str) -> str:
    return re.sub(r"\\(.)", lambda m: {"n": "\n", "t": "\t", "0": "\0"}.get(m.group(1), m.group(1)), s)


def rust_messages() -> list[str]:
    out: list[str] = []
    for f in RUST_FILES:
        if f.name in RUST_SKIP:
            continue
        src = re.split(r"#\[cfg\(test\)\]\s*mod ", f.read_text(encoding="utf-8"))[0]
        if f == CLI_HELP[0]:
            for line in src.split(CLI_HELP[1])[0].splitlines():
                doc = line.strip()
                if doc.startswith("///"):
                    text = doc[3:].strip()
                    if JA.search(text) and text not in out:
                        out.append(text)
        for line in src.splitlines():
            if line.strip().startswith("//"):
                continue
            code = line
            for m in LIT.findall(code):
                text = unescape(m)
                if text in RUST_NOT_MESSAGES or text.lstrip().startswith("<!doctype"):
                    continue
                if JA.search(text) and text not in out:
                    out.append(text)
    return out


def load(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def check(only: list[str] | None = None) -> int:
    errors: list[str] = []
    base = load(APP / "ja.json")
    ext_base = load(EXT / "ja" / "messages.json")
    for msg in rust_messages():
        if msg not in base["rust"]:
            errors.append(f"ja.json: rust 節にない Rust のメッセージ: {msg!r}（sync-rust を実行）")
    for lang, chrome_dirs in LANGS.items():
        if only and lang not in only:
            continue
        path = APP / f"{lang}.json"
        if not path.exists():
            errors.append(f"{path.name} がありません")
            continue
        data = load(path)
        for section, slot in (("ui", APP_SLOT), ("rust", RUST_SLOT)):
            want, got = base.get(section, {}), data.get(section, {})
            for k in want.keys() - got.keys():
                errors.append(f"{lang}.json: {section} にない: {k!r}")
            for k in got.keys() - want.keys():
                errors.append(f"{lang}.json: {section} の余分なキー: {k!r}")
            for k in want.keys() & got.keys():
                src = k if section == "rust" else want[k]
                v = got[k]
                if not isinstance(v, str) or not v.strip():
                    errors.append(f"{lang}.json: {section} が空: {k!r}")
                    continue
                if CONTROL.search(v):
                    errors.append(f"{lang}.json: 制御文字が入っている: {k!r}")
                # コマンドラインの見出し（「パスワード: 」など）は、前後の空白で揃えている
                if section == "rust" and (src[:len(src) - len(src.lstrip())] != v[:len(v) - len(v.lstrip())]
                                          or src[len(src.rstrip()):] != v[len(v.rstrip()):]):
                    errors.append(f"{lang}.json: 前後の空白が原文と違う: {k!r}: {v!r}")
                if sorted(slot.findall(src)) != sorted(slot.findall(v)) and section == "ui":
                    errors.append(f"{lang}.json: 差し込み口が違う: {k!r}: {v!r}")
                if section == "rust" and len(slot.findall(src)) != len(slot.findall(v)):
                    errors.append(f"{lang}.json: 差し込み口の数が違う: {k!r}: {v!r}")
                if sorted(TAG.findall(src)) != sorted(TAG.findall(v)):
                    errors.append(f"{lang}.json: タグが違う: {k!r}: {v!r}")
        if data.get("meta", {}).get("code") != lang:
            errors.append(f"{lang}.json: meta.code が {lang} ではない")
        for d in chrome_dirs:
            p = EXT / d / "messages.json"
            if not p.exists():
                errors.append(f"extension/_locales/{d}/messages.json がありません")
                continue
            ext = load(p)
            for k in ext_base.keys() - ext.keys():
                errors.append(f"{d}: ない: {k}")
            for k in ext.keys() - ext_base.keys():
                errors.append(f"{d}: 余分なキー: {k}")
            for k in ext_base.keys() & ext.keys():
                m = ext[k].get("message", "")
                if not m.strip():
                    errors.append(f"{d}: 空: {k}")
                elif CONTROL.search(m):
                    errors.append(f"{d}: 制御文字が入っている: {k}")
                elif sorted(EXT_SLOT.findall(ext_base[k]["message"])) != sorted(EXT_SLOT.findall(m)):
                    errors.append(f"{d}: 差し込み口が違う: {k}: {m!r}")
            desc = ext.get("ext_description", {}).get("message", "")
            if len(desc) > 132:
                errors.append(f"{d}: ext_description が 132 文字を超えている（{len(desc)} 文字）")
    for k in ext_base:
        if not re.fullmatch(r"[A-Za-z0-9_]+", k):
            errors.append(f"拡張のキーに使えない文字: {k}")
    for e in errors:
        print("✕", e)
    print(f"{'問題なし' if not errors else str(len(errors)) + ' 件の問題'}（言語 {len(LANGS)}、"
          f"アプリ {len(base['ui'])} 件 + Rust {len(base['rust'])} 件、拡張 {len(ext_base)} 件）")
    return 1 if errors else 0


def sync_rust() -> None:
    path = APP / "ja.json"
    data = load(path)
    msgs = rust_messages()
    data["rust"] = {m: m for m in msgs}
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"ja.json の rust 節を {len(msgs)} 件にしました")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "check"
    if cmd == "rust":
        for m in rust_messages():
            print(json.dumps(m, ensure_ascii=False))
    elif cmd == "sync-rust":
        sync_rust()
    else:
        # check のあとに言語コードを並べると、その言語だけを検査する（例: check fr de）
        sys.exit(check(sys.argv[2:] or None))
