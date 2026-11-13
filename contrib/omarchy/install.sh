#!/bin/bash
# Install the Omarchy integration: the dictation adapter and its installer/remover in ~/.local/bin, and
# WisprCheap in Omarchy's menu (Setup > Defaults > Dictation, and Remove > Dictation).
# It doesn't switch the dictation backend: pick WisprCheap in the menu for that.
set -e

here="$(cd "$(dirname "$0")" && pwd)"
bin="$HOME/.local/bin"
menu="${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/extensions/omarchy-menu.jsonc"

install -Dm755 -t "$bin" \
  "$here/omarchy-dictation-wisprcheap" \
  "$here/omarchy-install-dictation-wisprcheap" \
  "$here/omarchy-remove-dictation-wisprcheap"
echo "Installed the Omarchy dictation scripts in $bin."

if [[ ! -f $menu ]]; then
  mkdir -p "$(dirname "$menu")"
  printf '{\n}\n' > "$menu"
fi
if grep -q '"setup.default.dictation.wisprcheap"' "$menu"; then
  echo "The menu entries are already in $menu."
else
  cp "$menu" "$menu.bak.$(date +%s)"
  # Insert the entries before the file's closing brace (JSONC allows the trailing commas).
  python3 - "$menu" "$here/omarchy-menu.jsonc" <<'PY'
import sys
path, snippet = sys.argv[1], open(sys.argv[2], encoding="utf-8").read()
text = open(path, encoding="utf-8").read()
end = text.rstrip().rfind("}")
if end < 0:
    sys.exit(f"{path}: no closing brace")
open(path, "w", encoding="utf-8").write(text[:end].rstrip() + "\n" + snippet + text[end:])
PY
  echo "Added WisprCheap to Omarchy's menu ($menu)."
fi
