#!/bin/bash
set -e

FUZZ_SECONDS=${1:-60}

targets=$(
  grep -rn "fn fuzz_" hashiverse-lib/src/ --include="*.rs" -l \
    | while read file; do
        mod_path=$(echo "$file" | sed 's|hashiverse-lib/src/||; s|/|::|g; s|\.rs$||')
        grep -oP 'fn \Kfuzz_\w+' "$file" | while read fn_name; do
            echo "${mod_path}::tests::bolero_fuzz::${fn_name}"
        done
      done
)

echo "=== Compiling and fuzzing all targets in parallel for ${FUZZ_SECONDS}s each ==="
echo "$targets" | parallel --halt now,fail=1 --jobs 0 --line-buffer \
  "echo '=== Fuzzing {} ===' && timeout ${FUZZ_SECONDS} cargo bolero test -p hashiverse-lib '{}' || true"
echo "=== All fuzzers finished ==="
