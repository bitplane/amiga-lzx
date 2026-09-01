# Bump the patch version, verify it, commit, tag, and publish the refs.
release:
    #!/usr/bin/env bash
    set -euo pipefail
    status=$(git status --porcelain)
    if [[ -n "$status" ]]; then
        echo "error: worktree must be clean before releasing" >&2
        exit 1
    fi
    current=$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml | sed -n 's/^version = "\([0-9][0-9]*\)\.\([0-9][0-9]*\)\.\([0-9][0-9]*\)"$/\1.\2.\3/p' | head -n1)
    if [[ -z "$current" ]]; then
        echo "error: could not read the workspace version from Cargo.toml" >&2
        exit 1
    fi
    IFS=. read -r major minor patch <<< "$current"
    version="$major.$minor.$((patch + 1))"
    tag="v$version"
    if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
        echo "error: tag $tag already exists" >&2
        exit 1
    fi
    sed -i "0,/^version = \"$current\"$/s//version = \"$version\"/" Cargo.toml
    sed -i "s/version = \"$current\"/version = \"$version\"/" crates/amiga-lzx-cli/Cargo.toml
    cargo check --workspace
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace --all-targets
    git add Cargo.toml crates/amiga-lzx-cli/Cargo.toml Cargo.lock
    git commit -m "release $version"
    cargo publish --dry-run -p amiga-lzx
    cargo publish --dry-run -p amiga-lzx-cli
    git tag -a "$tag" -m "$tag"
    git push origin HEAD
    git push origin "$tag"
    echo "released $tag"
