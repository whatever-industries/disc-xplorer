#!/usr/bin/env bash
set -euo pipefail

tag=$1
commit=$2

retry() {
  local delay
  for delay in 0 3 10 20; do
    if (( delay )); then sleep "$delay"; fi
    if "$@"; then return 0; fi
  done
  return 1
}

ensure_draft() {
  local state
  state=$(gh api --paginate "repos/$GITHUB_REPOSITORY/releases?per_page=100" \
    --jq '.[] | select(.tag_name == env.TAG) | .draft') || return 1
  if [[ $state == false ]]; then
    echo "Release $tag is already public." >&2
    return 1
  fi
  if [[ $state == true ]]; then return 0; fi
  local prerelease=false
  if [[ $tag == *-* ]]; then prerelease=true; fi
  gh release create "$tag" --draft --target "$commit" \
    --title "Disc Xplorer $tag" --notes-file RELEASE_NOTES.md --prerelease="$prerelease"
}

export TAG="$tag"
export GH_REPO="$GITHUB_REPOSITORY"
retry ensure_draft
uploaded=$(retry gh release view "$tag" --json isDraft,assets)
jq -e '.isDraft' <<< "$uploaded" > /dev/null

for suffix in \
  "macOS_ARM_$tag.zip" "macOS_x64_$tag.zip" \
  "Windows_x64_$tag.exe" "Windows_ARM_$tag.exe" \
  "Linux_x64_$tag.AppImage" "Linux_ARM_$tag.AppImage" \
  "macOS_ARM_$tag.app.tar.gz" "macOS_x64_$tag.app.tar.gz" \
  "Linux_x64_$tag.AppImage.tar.gz" "Linux_ARM_$tag.AppImage.tar.gz" \
  latest.json; do
  asset="Disc Xplorer_$suffix"
  if [[ $suffix == latest.json ]]; then asset=$suffix; fi
  name=${asset// /.}
  digest=$(sha256sum < "$asset")
  if jq -e --arg name "$name" --arg digest "sha256:${digest%% *}" \
    '.assets | any(.name == $name and .digest == $digest and .state == "uploaded")' \
    <<< "$uploaded" > /dev/null; then
    echo "Already uploaded and verified $name"
    continue
  fi
  # Upload with the API's final filename to avoid a second request to restore its label.
  if [[ $asset != "$name" ]]; then ln -sf -- "$asset" "$name"; fi
  retry gh release upload "$tag" "$name" --clobber
done
