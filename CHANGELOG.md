# Changelog

## 0.8.0

### Breaking

- **`ddic show` is now `object show`**, and the `ddic` command group is gone. The old group held
  one command, and "DDIC" named a category that spans most of what Fractal already reads.
  `fractal ddic show ZFIELD` becomes `fractal object show ZFIELD`.

### Fixed

Three of these returned a wrong answer without reporting an error.

- **Table and structure field lists were incomplete.** A table's DDL source is not its field list:
  an `include` names a structure whose fields are spliced in but never shown, and an append
  structure does not appear in the source at all. `T005T` reported 7 of its 8 fields and said
  nothing about the eighth, so a write to `prq_spregt` was refused as an unknown field; tables
  carrying an include (`MARA`, `T001`, `BKPF`) failed to parse entirely. Fields now come from
  `DD03L`, where both kinds are already flattened.
- **`object source` could not say which layer it returned.** `--version inactive` on an object with
  nothing staged was served the *active* source under an `inactive` label. A source response cannot
  answer this — the HTTP headers for both layers are byte-identical — so the command now reads the
  object's document as well and reports the version that arrived, plus who last changed it. Costs
  one extra request; `edit read` deliberately does not pay it.
- **A class whose components sat in several tasks could not be edited.** Sending `corrNr` on the
  editing lock makes SAP validate the whole object's task membership up front, and refuse with
  "object components locked in different tasks" when any component belongs to a task under a
  different request. The lock is now taken bare and `corrNr` sent only on the source write, which is
  what Eclipse does. The change is still recorded against the transport, and SAP still enforces
  membership — on the write, where the same diagnostic fires.
- **The `object xml` hash could not be used as a token.** It was taken over the returned *page*, so
  `--limit 50` produced a hash `edit set-xml --expected-sha256` could never match, and it moved
  whenever anybody staged or discarded pending work, because ADT rewrites `atom:link` etags. It is
  now taken over the whole document with those links stripped — the same thing the write and journal
  paths hash. It is a token, not a checksum of the printed XML.
- **Column type handling corrected for older releases.** SAP serves `colType` only from SAP_BASIS
  758; on 757 the attribute arrives present but empty. Client-column detection and NUMC padding no
  longer depend on a field that is absent on most systems.

### Added

- **`object show` reads structures, tables, append structures and DDIC views.** Fields are
  flattened through includes and appends, and each one reports its data element, domain, DDIC type,
  ABAP runtime type, length, check table and description. The object is labelled as what `DD02L`
  says it is rather than as whatever collection served it — ADT's own `adtcore:type` reflects the
  URI you asked through, so the same table calls itself a structure when fetched that way.
- **`table data --types`** adds each column's data element, DDIC type and key flag to the output.
  Opt-in, and JSON only: it costs an extra read, and the readable grid has nowhere to show it.

### Internal

- Integration tests build as one binary instead of 31, and the shared ADT mock is compiled once
  rather than eight times.
- `object show` and `table metadata` now share a single field reader, so they can no longer drift.
  The data preview left the metadata path entirely; descriptions come from `DD04T` on both.

### Known gaps

- The lock change is covered by tests but has not been confirmed against a live system: the class
  that prompted it was held by a stray `EU510` enqueue that blocks every lock attempt regardless.
- `object show` still cannot read a CDS view (`DDLS`); it reads the classic DDIC view generated
  behind one.
