#!/bin/bash
set -euo pipefail

usage() {
    echo "Usage: $0 <static-site-directory> <updates-directory> <output-directory>" >&2
}

[[ $# -eq 3 ]] || {
    usage
    exit 2
}

SITE_OUTPUT="$1"
UPDATES="$2"
OUTPUT="$3"

for source_directory in "$SITE_OUTPUT" "$UPDATES"; do
    [[ -d "$source_directory" && ! -L "$source_directory" ]] || {
        echo "Pages artifact source must be a real directory: $source_directory" >&2
        exit 1
    }
    unsafe_link="$(find "$source_directory" -type l -print -quit)"
    [[ -z "$unsafe_link" ]] || {
        echo "Pages artifact source contains a symbolic link: $unsafe_link" >&2
        exit 1
    }
done

[[ -f "$SITE_OUTPUT/index.html" && ! -L "$SITE_OUTPUT/index.html" ]]
[[ -f "$SITE_OUTPUT/.nojekyll" && ! -L "$SITE_OUTPUT/.nojekyll" ]]
[[ -f "$UPDATES/channels/stable.json" && ! -L "$UPDATES/channels/stable.json" ]]
[[ -f "$UPDATES/public-key.txt" && ! -L "$UPDATES/public-key.txt" ]]
[[ -d "$UPDATES/releases" && ! -L "$UPDATES/releases" ]]

for reserved_path in channels releases public-key.txt; do
    [[ ! -e "$SITE_OUTPUT/$reserved_path" && ! -L "$SITE_OUTPUT/$reserved_path" ]] || {
        echo "Static site conflicts with the updates tree: $reserved_path" >&2
        exit 1
    }
done

if [[ -e "$OUTPUT" || -L "$OUTPUT" ]]; then
    [[ -d "$OUTPUT" && ! -L "$OUTPUT" ]] || {
        echo "Pages artifact output must be a real directory: $OUTPUT" >&2
        exit 1
    }
    [[ -z "$(find "$OUTPUT" -mindepth 1 -print -quit)" ]] || {
        echo "Pages artifact output directory must be empty: $OUTPUT" >&2
        exit 1
    }
else
    mkdir -p -- "$OUTPUT"
fi

cp -R "$SITE_OUTPUT"/. "$OUTPUT"/
cp -R "$UPDATES"/. "$OUTPUT"/

cmp -s "$SITE_OUTPUT/index.html" "$OUTPUT/index.html"
cmp -s "$UPDATES/channels/stable.json" "$OUTPUT/channels/stable.json"
cmp -s "$UPDATES/public-key.txt" "$OUTPUT/public-key.txt"
[[ -z "$(find "$OUTPUT" -type l -print -quit)" ]]

echo "Pages artifact assembled: $OUTPUT"
