# Sourced by build-release-cross.sh: repo_slug <remote-url> prints owner/repo for a GitHub remote, or fails.
# Accepts git@github.com:o/r(.git), ssh://git@github.com/o/r(.git), https://github.com/o/r(.git), with or without a trailing slash.
repo_slug() {
  slug=$(printf '%s\n' "$1" | sed -E 's#^(git@github\.com:|ssh://([^@/]+@)?github\.com(:[0-9]+)?/|https://([^@/]+@)?github\.com/)##; s#/$##; s#\.git$##')
  printf '%s\n' "$slug" | grep -Eq '^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$' || return 1
  printf '%s\n' "$slug"
}
