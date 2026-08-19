#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<'EOF'
Compare one returned preview value with a length computed by SAP for the same row.

Usage:
  scripts/probe-preview-server-length.sh \
    --table TABLE \
    --column COLUMN \
    --where SQL_WHERE_FRAGMENT \
    --length-expression SQL_EXPRESSION \
    --unit bytes|characters \
    [--profile PROFILE] \
    [--fractal-bin PATH] \
    [--show-value]

The expression must return a decimal integer. It can be a companion length column,
or a calculated expression such as LENGTH(TEXT) when the ADT freestyle endpoint and
column type support it. Some endpoints accept physical column names but reject
calculated select expressions. This script sends one read-only SELECT through
`fractal query` and requests at most one row.

For --unit bytes, the returned value must be represented as even-length hexadecimal;
the script compares half its displayed character count with the server length.

FRACTAL_BIN may be used instead of --fractal-bin. By default, this script uses
target/debug/fractal from the repository root, then falls back to PATH.
EOF
}

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 2
}

table=''
column=''
where_clause=''
length_expression=''
unit=''
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
        --length-expression)
            (($# >= 2)) || fail '--length-expression requires a value'
            length_expression=$2
            shift 2
            ;;
        --unit)
            (($# >= 2)) || fail '--unit requires a value'
            unit=$2
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
[[ -n $where_clause ]] || fail '--where is required so the comparison targets one row'
[[ -n $length_expression ]] || fail '--length-expression is required'
[[ $unit == bytes || $unit == characters ]] || fail '--unit must be bytes or characters'
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

probe_tmp=$(mktemp -d "${TMPDIR:-/tmp}/fractal-preview-length-probe.XXXXXX")
trap 'rm -rf "$probe_tmp"' EXIT
json_file=$probe_tmp/response.json
value_file=$probe_tmp/value

query="SELECT $column AS PROBE_VALUE, $length_expression AS PROBE_LENGTH FROM $table WHERE $where_clause"
command_args=("$fractal_bin" --output json)
if [[ -n $profile ]]; then
    command_args+=(--profile "$profile")
fi
command_args+=(query "$query" --limit 1)

if ! "${command_args[@]}" >"$json_file"; then
    if jq -e . "$json_file" >/dev/null 2>&1; then
        jq '{ok, code, message, hint}' "$json_file" >&2
    fi
    fail 'the Fractal command failed'
fi

if ! jq -e '.ok == true' "$json_file" >/dev/null; then
    jq '{ok, code, message, hint}' "$json_file" >&2
    fail 'SAP returned an error; check whether the length expression is supported in SQL'
fi

row_count=$(jq '.rows | length' "$json_file")
[[ $row_count -eq 1 ]] || fail "expected one row, received $row_count; make --where more specific"

value_index=$(jq -r '.columns | map(.name | ascii_upcase) | index("PROBE_VALUE") // empty' "$json_file")
length_index=$(jq -r '.columns | map(.name | ascii_upcase) | index("PROBE_LENGTH") // empty' "$json_file")
[[ -n $value_index ]] || fail 'SAP did not return the PROBE_VALUE alias'
[[ -n $length_index ]] || fail 'SAP did not return the PROBE_LENGTH alias'

jq -j --argjson index "$value_index" '.rows[0][$index]' "$json_file" >"$value_file"
returned_chars=$(jq --argjson index "$value_index" '.rows[0][$index] | length' "$json_file")
returned_bytes=$(wc -c <"$value_file" | tr -d '[:space:]')
server_length=$(jq -r --argjson index "$length_index" '.rows[0][$index]' "$json_file")
[[ $server_length =~ ^[0-9]+$ ]] || fail "the length expression returned a non-integer value: $server_length"
sha256=$(LC_ALL=C shasum -a 256 "$value_file" | awk '{print $1}')

printf 'table: %s\n' "$table"
printf 'column: %s\n' "$column"
printf 'server_reported_%s: %s\n' "$unit" "$server_length"
printf 'returned_json_characters: %s\n' "$returned_chars"
printf 'returned_json_utf8_bytes: %s\n' "$returned_bytes"
printf 'returned_value_sha256: %s\n' "$sha256"

if [[ $show_value == true ]]; then
    printf 'returned_value:\n'
    jq -r --argjson index "$value_index" '.rows[0][$index]' "$json_file"
fi

if [[ $unit == bytes ]]; then
    hex_source_bytes=$(jq -r --argjson index "$value_index" '
        .rows[0][$index]
        | if test("^[0-9A-Fa-f]*$") and ((length % 2) == 0)
          then (length / 2 | tostring)
          else ""
          end
    ' "$json_file")
    [[ -n $hex_source_bytes ]] || fail 'byte comparison requires an even-length hexadecimal response'
    observed_length=$hex_source_bytes
    printf 'returned_hex_candidate_bytes: %s\n' "$observed_length"
else
    observed_length=$returned_chars
fi

if [[ $observed_length -eq $server_length ]]; then
    printf 'comparison: equal\n'
elif [[ $observed_length -lt $server_length ]]; then
    printf 'comparison: returned-value-is-shorter\n'
    exit 3
else
    printf 'comparison: returned-value-is-longer\n'
    exit 3
fi
