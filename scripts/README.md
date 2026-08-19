# Live preview probes

These scripts measure one table-preview cell without printing its value by default.
They are intended to answer whether the SAP preview response Fractal receives is
shorter than the source value. They only perform read-only table preview or query
requests.

Build Fractal first and make sure `jq` is installed:

```sh
cargo build
```

## Check against a known size

Use `probe-preview-cell.sh` when you already know the exact size of a test value:

```sh
scripts/probe-preview-cell.sh \
  --table Z_EXAMPLE \
  --column PAYLOAD \
  --where "ID = 'TEST_ROW'" \
  --expected-source-bytes 4096
```

`--expected-source-bytes` works when SAP represents the value as hexadecimal. For
ordinary text, use `--expected-json-chars` or `--expected-json-bytes`. The script
exits with status 3 if the observed size differs from the expected size.

## Check against a server-computed size

Use `probe-preview-server-length.sh` when SAP can calculate the original length in
the same query:

```sh
scripts/probe-preview-server-length.sh \
  --table Z_EXAMPLE \
  --column PAYLOAD \
  --where "ID = 'TEST_ROW'" \
  --length-expression "PAYLOAD_BYTE_LENGTH" \
  --unit bytes
```

The length expression can name a companion column that stores the source size. For a
text column, a system whose ADT freestyle endpoint accepts calculated select
expressions can instead use ABAP SQL's `LENGTH` function:

```sh
scripts/probe-preview-server-length.sh \
  --table Z_EXAMPLE \
  --column LONG_TEXT \
  --where "ID = 'TEST_ROW'" \
  --length-expression "LENGTH(LONG_TEXT)" \
  --unit characters
```

Some ADT backends reject calculated expressions here and report the whole expression
as an unknown column. In that case, use a known expected size with
`probe-preview-cell.sh` or provide a physical companion length column.

If the binary response is not hexadecimal, the script reports that it cannot make a
byte comparison instead of guessing how SAP encoded it.

Pass `--profile PROFILE` when needed. Both scripts also accept `--fractal-bin PATH`,
and `--show-value` if displaying the potentially sensitive cell is intentional.

These probes distinguish Fractal's readable rendering from its JSON output. The
readable grid shortens cells for display, while JSON serializes the complete string
that Fractal parsed from SAP. A shorter JSON value therefore indicates that the
preview response was already short (or that the chosen server-length expression
uses different units), not that the JSON renderer shortened it.
