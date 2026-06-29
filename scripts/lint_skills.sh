#!/usr/bin/env bash
# Lint the agent skill catalog shape: root SKILL.md and each skills/*/SKILL.md
# must have YAML frontmatter with a non-empty `name` and `description`.
# Flag-free by design — the lint checks the file structure, not the flag content
# (flags live in `nbox <cmd> --help` and can't drift here).
#
# Usage: scripts/lint_skills.sh
# Exit 0 if all skill files pass, 1 otherwise.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
skills_dir="$root/skills"
errors=0

if [ ! -d "$skills_dir" ]; then
    echo "error: $skills_dir not found" >&2
    exit 1
fi

# Check the root skill plus every SKILL.md under skills/.
while IFS= read -r -d '' skill_file; do
    # Must start with `---` frontmatter.
    if ! head -1 "$skill_file" | grep -q '^---$'; then
        echo "error: $skill_file: missing YAML frontmatter (must start with ---)" >&2
        errors=$((errors + 1))
        continue
    fi

    # Extract frontmatter (between the first and second `---` lines).
    frontmatter=$(sed -n '1,/^---$/p' "$skill_file" | sed '1d;$d')

    # Must have a non-empty `name:`.
    if ! echo "$frontmatter" | grep -q '^name: .\+'; then
        echo "error: $skill_file: frontmatter missing non-empty 'name:'" >&2
        errors=$((errors + 1))
    fi

    # Must have a non-empty `description:`.
    if ! echo "$frontmatter" | grep -q '^description: .\+'; then
        echo "error: $skill_file: frontmatter missing non-empty 'description:'" >&2
        errors=$((errors + 1))
    fi
done < <(
    printf '%s\0' "$root/SKILL.md"
    find "$skills_dir" -name 'SKILL.md' -print0
)

if [ "$errors" -gt 0 ]; then
    echo "error: $errors skill file(s) failed lint" >&2
    exit 1
fi

echo "ok: all skill files pass lint"
