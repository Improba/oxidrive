#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

BUMP_TYPE="${1:-patch}"

if [[ "$BUMP_TYPE" != "major" && "$BUMP_TYPE" != "minor" && "$BUMP_TYPE" != "patch" ]]; then
  echo "Error: argument must be 'major', 'minor', or 'patch' (got '$BUMP_TYPE')" >&2
  exit 1
fi

CURRENT_VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')

IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT_VERSION"

case "$BUMP_TYPE" in
  major) MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0 ;;
  minor) MINOR=$((MINOR + 1)); PATCH=0 ;;
  patch) PATCH=$((PATCH + 1)) ;;
esac

NEW_VERSION="${MAJOR}.${MINOR}.${PATCH}"

echo "Current version: $CURRENT_VERSION"
echo "Bump type: $BUMP_TYPE"
echo "New version: $NEW_VERSION"

sed -i "s/^version = \"$CURRENT_VERSION\"/version = \"$NEW_VERSION\"/" Cargo.toml
echo "Updated Cargo.toml"

git add Cargo.toml
git commit -m "chore: bump version to v${NEW_VERSION}"
echo "Committed: chore: bump version to v${NEW_VERSION}"

git tag "v${NEW_VERSION}"
echo "Tagged: v${NEW_VERSION}"

echo "Run 'git push && git push origin v${NEW_VERSION}' to publish"
