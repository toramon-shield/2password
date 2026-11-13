#!/usr/bin/env python3
"""Chrome ウェブストアに提出する ZIP を作る。

使い方: python scripts/package-extension.py
出力  : dist/2password-extension-<version>.zip

開発版の ID を固定するための `key` は、ストアに提出するパッケージには入れられないので取り除く。
（ストア版の ID はストアが決める。決まった ID はアプリ側の STORE_EXTENSION_IDS に追加する）
"""
import json
import pathlib
import zipfile

root = pathlib.Path(__file__).resolve().parent.parent
src = root / "extension"
manifest = json.loads((src / "manifest.json").read_text(encoding="utf-8"))
manifest.pop("key", None)

out_dir = root / "dist"
out_dir.mkdir(exist_ok=True)
out = out_dir / f"2password-extension-{manifest['version']}.zip"

with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("manifest.json", json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    for path in sorted(src.rglob("*")):
        if path.is_file() and path.name != "manifest.json":
            z.write(path, path.relative_to(src).as_posix())

print(out)
