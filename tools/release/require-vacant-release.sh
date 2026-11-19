#!/bin/sh
# List every authenticated release page, including unpublished drafts.
set -eu
repository=${1:?repository required}
tag=${2:?release tag required}
pages=$(gh api --paginate --slurp -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' "repos/$repository/releases?per_page=100") || {
    echo "could not prove GitHub Release $tag is vacant" >&2
    exit 1
}
printf '%s\n' "$pages" | jq --exit-status --arg tag "$tag" '
    type == "array" and length > 0 and all(.[];
        type == "array" and all(.[]; (.tag_name | type == "string") and .tag_name != $tag))' >/dev/null || {
    echo "GitHub Release $tag already exists or its inventory is invalid" >&2
    exit 1
}
