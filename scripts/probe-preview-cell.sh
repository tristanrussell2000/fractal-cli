#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<'EOF'
Measure one cell exactly as Fractal receives it from SAP's table preview endpoint.

Usage:
  scripts/probe-preview-cell.sh \
    --table TABLE \
    --column COLUMN \
    [--where SQL_WHERE_FRAGMENT] \
    [--expected-json-chars N] \
    [--expected-json-bytes N] \
    [--expected-source-bytes N] \
    [--profile PROFILE] \
    [--fractal-bin PATH] \
    [--show-value]

The value is not printed unless --show-value is supplied. The source-byte check is
only available when the returned representation consists of an even number of hex
digits, as is commonly the case for RAW-like values.

FRACTAL_BIN may be used instead of --fractal-bin. By default, this script uses
target/debug/fractal from the repository root, then falls back to PATH.
EOF
}

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 2
}

require_nonnegative_integer() {
    local option_name=$1
    local value=$2

    [[ $value =~ ^[0-9]+$ ]] || fail "$option_name must be a non-negative integer"
}

table=''
column=''
where_clause=''
expected_json_chars=''
expected_json_bytes=''
expected_source_bytes=''
profile=''
fractal_bin=${FRACTAL_BIN:-}
show_value=false

while (($# > 0)); do
    case "$1" in
        --table)
            (($# >= 2)) || fail '--table requires a value'
            table=$2
            shift 2
            ;;
        --column)
            (($# >= 2)) || fail '--column requires a value'
            column=$2
            shift 2
            ;;
        --where)
            (($# >= 2)) || fail '--where requires a value'
            where_clause=$2
            shift 2
            ;;
        --expected-json-chars)
            (($# >= 2)) || fail '--expected-json-chars requires a value'
            expected_json_chars=$2
            require_nonnegative_integer "$1" "$2"
            shift 2
            ;;
        --expected-json-bytes)
            (($# >= 2)) || fail '--expected-json-bytes requires a value'
            expected_json_bytes=$2
            require_nonnegative_integer "$1" "$2"
            shift 2
            ;;
        --expected-source-bytes)
            (($# >= 2)) || fail '--expected-source-bytes requires a value'
            expected_source_bytes=$2
            require_nonnegative_integer "$1" "$2"
            shift 2
            ;;
        --profile)
            (($# >= 2)) || fail '--profile requires a value'
            profile=$2
            shift 2
            ;;
        --fractal-bin)
            (($# >= 2)) || fail '--fractal-bin requires a value'
            fractal_bin=$2
            shift 2
            ;;
        --show-value)
            show_value=true
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
done

[[ -n $table ]] || fail '--table is required'
[[ -n $column ]] || fail '--column is required'
command -v jq >/dev/null 2>&1 || fail 'jq is required'
command -v shasum >/dev/null 2>&1 || fail 'shasum is required'

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/.." && pwd)

if [[ -z $fractal_bin ]]; then
    if [[ -x $repo_root/target/debug/fractal ]]; then
        fractal_bin=$repo_root/target/debug/fractal
    elif command -v fractal >/dev/null 2>&1; then
        fractal_bin=$(command -v fractal)
    else
        fail 'Fractal was not found; run cargo build or pass --fractal-bin'
    fi
fi

[[ -x $fractal_bin ]] || fail "Fractal is not executable: $fractal_bin"

probe_tmp=$(mktemp -d "${TMPDIR:-/tmp}/fractal-preview-probe.XXXXXX")
trap 'rm -rf "$probe_tmp"' EXIT
json_file=$probe_tmp/response.json
value_file=$probe_tmp/value

command_args=("$fractal_bin" --output json)
if [[ -n $profile ]]; then
    command_args+=(--profile "$profile")
fi
command_args+=(table data "$table" --fields "$column" --limit 1)
if [[ -n $where_clause ]]; then
    command_args+=(--where "$where_clause")
fi

if ! "${command_args[@]}" >"$json_file"; then
    if jq -e . "$json_file" >/dev/null 2>&1; then
        jq '{ok, code, message, hint}' "$json_file" >&2
    fi
    fail 'the Fractal command failed'
fi

if ! jq -e '.ok == true' "$json_file" >/dev/null; then
    jq '{ok, code, message, hint}' "$json_file" >&2
    fail 'SAP returned an error'
fi

row_count=$(jq '.rows | length' "$json_file")
[[ $row_count -eq 1 ]] || fail "expected one row, received $row_count; use --where to select one row"

column_count=$(jq '.rows[0] | length' "$json_file")
[[ $column_count -eq 1 ]] || fail "expected one returned column, received $column_count"

jq -j '.rows[0][0]' "$json_file" >"$value_file"
json_chars=$(jq '.rows[0][0] | length' "$json_file")
json_bytes=$(wc -c <"$value_file" | tr -d '[:space:]')
sha256=$(LC_ALL=C shasum -a 256 "$value_file" | awk '{print $1}')
hex_source_bytes=$(jq -r '
    .rows[0][0]
    | if test("^[0-9A-Fa-f]*$") and ((length % 2) == 0)
      then (length / 2 | tostring)
      else ""
      end
' "$json_file")

printf 'table: %s\n' "$table"
printf 'column: %s\n' "$column"
printf 'returned_json_characters: %s\n' "$json_chars"
printf 'returned_json_utf8_bytes: %s\n' "$json_bytes"
if [[ -n $hex_source_bytes ]]; then
    printf 'hex_candidate_source_bytes: %s\n' "$hex_source_bytes"
else
    printf 'hex_candidate_source_bytes: not-applicable\n'
fi
printf 'returned_value_sha256: %s\n' "$sha256"

if [[ $show_value == true ]]; then
    printf 'returned_value:\n'
    jq -r '.rows[0][0]' "$json_file"
fi

comparison_failed=false

compare_exactly() {
    local label=$1
    local observed=$2
    local expected=$3

    if [[ $observed -eq $expected ]]; then
        printf '%s_check: equal\n' "$label"
    elif [[ $observed -lt $expected ]]; then
        printf '%s_check: shorter-than-expected (expected %s)\n' "$label" "$expected"
        comparison_failed=true
    else
        printf '%s_check: longer-than-expected (expected %s)\n' "$label" "$expected"
        comparison_failed=true
    fi
}

if [[ -n $expected_json_chars ]]; then
    compare_exactly json_characters "$json_chars" "$expected_json_chars"
fi
if [[ -n $expected_json_bytes ]]; then
    compare_exactly json_utf8_bytes "$json_bytes" "$expected_json_bytes"
fi
if [[ -n $expected_source_bytes ]]; then
    [[ -n $hex_source_bytes ]] || fail '--expected-source-bytes requires an even-length hexadecimal response'
    compare_exactly source_bytes "$hex_source_bytes" "$expected_source_bytes"
fi

if [[ $comparison_failed == true ]]; then
    exit 3
fi
