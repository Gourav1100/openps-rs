#!/usr/bin/env bash
# Runs scene.py in this folder's Python environment, creating or updating it when requirements.txt is new (uv if there is one, else venv + pip).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
venv="$here/.venv"
stamp="$venv/.requirements"
want="$(shasum "$here/requirements.txt" | cut -d' ' -f1)"
if [[ ! -x "$venv/bin/python" || "$(cat "$stamp" 2>/dev/null || true)" != "$want" ]]; then
  echo "Setting up the Python environment in $venv (the first time takes a few minutes) ..." >&2
  if command -v uv >/dev/null; then
    [[ -x "$venv/bin/python" ]] || uv venv --python 3.12 "$venv"
    uv pip install --quiet --python "$venv/bin/python" -r "$here/requirements.txt"
  else
    [[ -x "$venv/bin/python" ]] || python3 -m venv "$venv"
    "$venv/bin/python" -m pip install --quiet -r "$here/requirements.txt"
  fi
  echo "$want" > "$stamp"
fi
exec "$venv/bin/python" "$here/scene.py" "$@"
