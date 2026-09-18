#!/usr/bin/env bash
# Downloads every package listed in packages/packages.txt from the Typst
# registry into packages/<namespace>/<name>/<version>/.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
list="$repo/packages/packages.txt"
root="$repo/packages"
registry="https://packages.typst.org"

specs=$(sed 's/#.*//' "$list" | cut -d'|' -f1 | sed 's/[[:space:]]//g' | grep -v '^$')

for spec in $specs; do
  rest="${spec#@}"
  namespace="${rest%%/*}"
  rest="${rest#*/}"
  name="${rest%%:*}"
  version="${rest##*:}"

  if [ "$namespace" != "preview" ]; then
    echo "error: only the @preview namespace can be downloaded ($spec)" >&2
    exit 1
  fi

  dest="$root/$namespace/$name/$version"
  if [ -f "$dest/typst.toml" ]; then
    echo "have $spec"
    continue
  fi

  echo "fetching $spec"
  rm -rf "$dest"
  mkdir -p "$dest"
  curl -sSfL "$registry/$namespace/$name-$version.tar.gz" | tar xz -C "$dest"
done

# Every @preview import inside a vendored package must itself be listed,
# otherwise it resolves at render time and the user gets a confusing error.
missing=$(grep -rhoE '"@preview/[a-z0-9_.-]+:[0-9]+\.[0-9]+\.[0-9]+' "$root" --include='*.typ' \
  | sed 's/^"//' | sort -u \
  | while read -r used; do
      grep -qF "$used" "$list" || echo "$used"
    done)

if [ -n "$missing" ]; then
  echo >&2
  echo "error: vendored packages import these, but packages.txt does not list them:" >&2
  echo "$missing" | sed 's/^/  /' >&2
  exit 1
fi

echo "ok: $(echo "$specs" | wc -l | tr -d ' ') packages vendored, dependency closure complete"
