#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

mkdir -p assets

CARGO_BIN="${CARGO:-cargo}"
CMD=("$CARGO_BIN" test generate_all_screenshots -- --ignored --nocapture --test-threads=1)

if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    if command -v xvfb-run >/dev/null 2>&1; then
        echo "No active display detected; running under xvfb-run..."
        xvfb-run -a "${CMD[@]}"
    else
        echo "Error: No DISPLAY or WAYLAND_DISPLAY set and xvfb-run is not installed." >&2
        exit 1
    fi
else
    "${CMD[@]}"
fi

SCREENSHOTS=(
    "assets/connection.png"
    "assets/content.png"
    "assets/structure.png"
    "assets/relations.png"
    "assets/query.png"
    "assets/users.png"
)

echo ""
echo "Updated screenshots:"
for img in "${SCREENSHOTS[@]}"; do
    if [[ ! -s "$img" ]]; then
        echo "Error: Expected screenshot '$img' was not generated or is empty." >&2
        exit 1
    fi
    echo "  - $img"
done
