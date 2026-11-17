# Changelog

## Unreleased

### Added

- **`fractal test`** runs ABAP Unit tests and says whether they are green, for a class, program,
  function group or package (with its sub-packages), several at once with repeated
  `--object TYPE:NAME`. On a class, `--test-class` and `--method` narrow the run to one local test
  class or some of its methods.
  - **Green** means at least one test passed and nothing failed, errored, was skipped, or blocked a
    whole test class (a failing `class_setup`) or program (syntax errors). A run narrowed to a
    selection also has to have run exactly that.
  - **Exit codes:** 0 when green; 1 when the run completed and is not green, with the failures in
    the output; 2 when it could not be run.
  - Each failure carries the include URI and line, which `edit read --include testclasses` opens.
  - A missing object is refused up front (`unit_test_target_not_found`), because SAP answers it
    exactly like an object without tests. An unknown `--test-class` or `--method` is refused with
    the class's real ones listed.
  - Harmless and dangerous tests, short and medium, run by default; `--risk` and `--duration` add
    critical and long. The run waits up to `--timeout` seconds (default 300).
  - `guard install` asks before it.

### Fixed

- **`delete` refused a class whose own local tests use it** (`edit_delete_object_in_use`, naming
  "Local Test Classes"). The 0.11.0 where-used change counted a hit inside the object's own
  includes as a usage. References from within the object itself are now ignored.

- **`guard install` let agents write table rows and run classes without asking.** `table set`,
  `insert` and `remove` and `internal class-run` are now on the ask list, in both the Claude Code
  rules and the Codex hook. The table writes are asked about even as a dry run, since a prefix rule
  cannot see `--execute`. Re-run `fractal guard install` to add the new rules.

## 0.11.0

### Added

- **`fractal edit activate --object TYPE:NAME --object TYPE:NAME …`** activates several source
  objects in one request. This is the only way to activate objects that need each other, such as a
  new root view with a `composition` and its child view with an `association to parent`: neither
  can be activated first. Each object is written type, colon, name (`--object
  DDLS:ZRAP_R_TRAVEL`); `--type`/`--name` stays the single-object form.
  - The group skips Fractal's own syntax pre-check, which cannot pass for such a pair. SAP still
    checks every object as it activates.
  - **SAP does not roll a group back.** With one object broken, it reports the activation
    cancelled, yet still publishes the others' source, leaving them listed as inactive. So each
    object is judged by its own state afterwards:
    - `activated`;
    - `published_still_listed`;
    - `not_activated`, with SAP's message;
    - `unverified`.

    Anything short of all activated is the error `edit_activation_incomplete`, and running the
    same command again after a fix finishes the group.
  - An object with nothing staged is left out, and named under `nothing_to_activate`.
  - Each object gets its own journal entry, so `undo` works per object. The entries share a
    `group` id, which `journal list` shows.
  - Source-based types only.

### Fixed

- **A failed read was taken to mean "there is nothing there"** in four places, each of which could
  leave the journal or an undo wrong:
  - the active version recorded before activating any source object other than a class;
  - the same for a metadata object (data element, domain, service binding…);
  - `undo`'s read of the current active version;
  - `undo`'s read of the current pending version, which guards your staged work.

  A 500 or a timeout was recorded as "never activated", so an undo would have treated that
  activation as the object's first; or it hid pending work an undo would then write over. Now only
  a 404 means absent. Anything else stops the activation (`edit_activation_before_image_unreadable`)
  or the undo (`undo_current_state_unreadable`) before anything is written.
- **Activation hints repeated SAP's messages**, such as "DDLS … was not activated" twice. Identical
  messages are now listed once.

- **`delete` let through objects that CDS views or classes still used.** The where-used guard
  counted only references SAP flags `isResult="true"`, and SAP leaves that flag false on a CDS
  view selecting from the object and on a class whose method uses it. So deleting a table under
  active views went ahead and broke them. Deleting one of two views that reference each other
  failed with a bare `400 … could not be deleted`. The guard now also counts what SAP grades a
  usage (`usageInformation`), and names each referencing object once, as the class rather than the
  method. `object usages --direct-results` lists those usages too.
- When `--force` overrides the guard and SAP still refuses, the hint now says SAP will not delete
  an object that active objects use, and names them, instead of a generic message.

## 0.10.0

### Breaking

- **A table write that did not write the row is now an error.** `table set`, `insert` and
  `remove` used to answer `ok: true` with the reason only in `status`, so an agent checking `ok`
  took a refusal for success. These now exit non-zero with `ok: false` and a code:
  `table_write_conflict`, `table_write_row_not_found`, `table_write_row_exists`,
  `table_write_row_locked`, `table_write_transport_refused`,
  `table_write_transport_key_too_long`, `table_write_transport_key_not_character`,
  `table_write_exception` or `table_write_not_applied`. The same goes for a dry run that would be
  refused. `status` on a success is now `applied`, `applied_with_transport_entry`,
  `applied_without_transport_entry` (with a `warning`: the row changed but the transport entry
  was refused) or `would_apply`.

### Added

- **`fractal package create`** creates a package and reads it back.
  - A `$NAME` package is local: it is never transported and takes no transport.
  - Any other package needs `--transport`, plus a software component and transport layer, which
    default to the parent's when `--parent` is given.
  - The name has to be in the customer namespaces and, on a profile with a package allowlist, on
    the allowlist.
  - `guard install` asks before it.
- **`fractal delete --type DEVC`** deletes a package, and only an empty one: any object or
  subpackage in it refuses the delete (`package_not_empty`) before it is locked. A transportable
  package whose objects were deleted under an unreleased request is refused by SAP. Fractal reports
  that as `package_pending_deletions` and says to release the request first. Package deletes are
  not journaled.

### Fixed

- **`undo` of a table write reported any refusal as "the row has moved".** A lock held by another
  session, or a refused transport entry, now surfaces as itself. Only a row that changed since the
  write is reported as a divergence.
- **A deleted object's name could not be reused for half an hour or more.** SAP's editing lock
  outlives the object it was taken on, and Fractal released it only when a delete failed, so
  recreating the same name was refused as "User … is currently editing" until the session timed
  out. The lock is now released after a successful delete too, for every object type and for
  packages. If that release fails, the delete is still reported done, with a `warning` that the
  name is held.
- **`package items` and `package tree` failed on an empty package** with
  `package_response_parse_error`. SAP answers an empty package and a missing one with the same
  empty body. An empty package now lists as empty, and a missing one is reported as
  `package_not_found`.

## 0.9.2

### Fixed

- **`table insert`, `set` and `remove` failed on a table with a RAW key**, such as a RAP table keyed
  on `sysuuid_x16`. The generated class did not activate and nothing was written. The lock key is
  now built the way SE16N builds it: the table's key fields, client included, cast byte for byte.
- **The table lock left out the client on an application table**, so it never collided with
  SE16N's lock on the same row. It now always includes it.
- **Key fields given out of table order produced a wrong lock and transport key** on a table with
  more than one key field. They are now put in the table's order.

### Changed

- A customizing table whose key is not all character fields (a RAW UUID, an integer) now refuses a
  transport entry with status `transport_key_not_character`, and writes nothing. SE16N would
  record a generic `*` key, which transports every row; Fractal does not widen an entry silently.

## 0.9.1

0.9.0 was tagged but never released: its Apple Silicon build could not get a runner after GitHub
retired the macos-14 image. 0.9.1 is the same release, built on macos-15.

### Added

- **`fractal binding get --name <SRVB> <PATH>`** reads from a published service. `PATH` is the
  part of the URL after the service root, written as in `/IWFND/GW_CLIENT` (`'$metadata'`,
  `'Items?$top=5&$count=true'`, `"Items('A1')?\$expand=_Child"`). Fractal takes the root from the
  service's registration, so nobody has to assemble V4's `srvd/…/0001/` form. It is GET-only,
  refuses a path that would leave the service root, and refuses an unpublished service up front
  (V2 would answer a misleading 403). The JSON output carries the URL, status, content type and
  body, which is parsed when SAP answers JSON.
- **SAP's message is now reported for OData JSON errors**, such as "Invalid token 'Nope' at
  position '1'" for a bad `$filter`, where before only "Bad Request" came through.
- **`fractal binding show | publish | unpublish`** — publish a service binding, the last step to a
  callable RAP service. OData V2 and V4 both work; the binding's own protocol decides which
  endpoints are used. Success is proved by reading the registration back, never by SAP's answer,
  which reports a refusal as HTTP 200. Publishing a published service, or unpublishing an
  unpublished one, sends nothing. `show` reports the service URL and entity sets, and the binding's
  own `published` flag beside the registration's, because the two have been seen to disagree.
  `show` works on any binding; `publish` and `unpublish` are limited to the customer namespaces.
  Publishing is not journaled: it changes no content, and unpublish is its inverse. It needs no
  transport, even for a binding in a transportable package; the gateway objects travel with the
  binding's activation.
  `guard install` asks before `binding publish` and `unpublish`.
- **Deleting a published binding says to unpublish it first** (`edit_delete_binding_published`)
  instead of a generic `edit_delete_request_failed`.
- **`--include` reads a class's other source streams**: `definitions` (class-relevant local
  types), `implementations` (local types, where RAP handlers live), `macros` and `testclasses`.
  On `edit read`, and on `object source`, which also accepts an include's own URI
  (`…/includes/implementations`). The version and author reported are that include's own, not the
  class's.
- **`edit set --include` and `edit patch --include` write a class's other includes.** Writing
  `testclasses` creates the include first if the class has none; a dry run says it would
  (`creates_include`). A patch never creates an include, and a missing one is reported as
  `edit_include_not_found`.

### Breaking

- **The journal's entry format changed, and old entries are not read.** An entry is now one type
  covering the operation, the object and its content, and a class records every include. Entries
  written by earlier versions are listed under `unreadable` by `journal list`; delete the journal
  directory to clear them.
- `fractal undo --output json` reports per-stream fields under `streams` instead of top-level
  `restore_sha256` and friends.
- `edit discard --output json` reports per-stream fields under `streams` (with `include` for a
  class) instead of top-level `discarded_sha256`, `active_sha256_before` and friends.

### Fixed

- **A multi-line `edit patch` anchor never matched ABAP source.** SAP serves ABAP with CRLF line
  endings, so a `--find` typed with LF reported `patch_anchor_not_found`. When the anchor does not
  match as given and the source uses one line-ending convention throughout, the anchor and the
  replacement are now rewritten to it; the output says so in `line_endings_adapted_to`. The
  replacement is always written in the source's convention, so a patch no longer leaves a source
  with mixed line endings. An exact match still wins, and a source that already mixes them is
  matched literally.
- **Writing `main` was refused while a colleague had any include staged.** The check for another
  user's staged edit looked at the class as a whole, whose marker goes inactive when any include is
  staged. It now looks at the include being written, so a colleague's staged local types no longer
  block a write to `main`, and a refusal names the include.

- **Undoing a class activation restored only `main`.** An activation publishes every include the
  caller has staged, local types included, and the journal recorded only `main`, so an undo
  reported success with the other includes still changed. Activations and deletes of a class now
  record every include, and undo restores each one.
- **A class activation failed verification when someone else was editing the class.** Activation
  publishes only the caller's staged includes, but verification always compared `main`, so a
  colleague's staged `main` made a successful activation report
  `edit_activation_verification_mismatch`. Each include the activation published is now verified
  instead, and `edit activate` reports `published` and `staged_by_others` for a class.
- **`edit discard` on a class published staged includes instead of discarding them.** It restored
  only `main`, then activated the class, which publishes every include the caller has staged.
  Every staged include is now restored first, and each is checked afterwards. Discard also no
  longer refuses when SAP drops a trailing line break on the restore; a line-ending change still
  refuses.
- **A class delete kept only `main`.** Its restore recipe now names each include's content.
- A journal entry that could not be read vanished from `journal list` without a word. It is now
  listed as unreadable.
- **Syntax-check messages had no line number.** SAP gives a message's position only in its URI
  (`…#start=9,9`), and Fractal read a `line` attribute SAP does not send. Messages now carry
  `line`, `column`, `uri` and, for a class, the `include` they are in — so an error in local types
  is no longer reported as if it were in the global class.

### Known gaps

- **`table insert`, `set` and `remove` fail on a table with a RAW key** (such as a RAP table keyed
  on `sysuuid_x16`): the generated class does not activate, and nothing is written. Seed such a
  table through its business object instead.

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
