# Project

dssr is a discovery backend for any OAI-PMH data provider, built first for a university library.
It harvests metadata over OAI-PMH, keeps its own tantivy search index, and serves search over an HTTP API, so client services never touch the source systems.
The README states how dssr relates to VuFind and which contracts it follows.

## Workflow guardrails

Never run `cargo build`, `cargo test`, `cargo fmt`, `git add`, or `git commit`.
End each code change with the exact command for the user to run instead:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
shellcheck install.sh scripts/package.sh
```

Run read-only commands in their non-interactive form, because a pager blocks the session: `git --no-pager diff`, `git --no-pager log`, `journalctl --no-pager`, `systemctl --no-pager status`.

## Toolchain

dssr compiles on stable `rustc` only.
Never add `#![feature(...)]`, `cargo +nightly`, or any unstable language or library feature.
`Cargo.toml` uses `edition = "2024"`, and `rust-version` holds the minimum stable release that builds the crate with its dependencies; raise it when a change needs a newer stable feature.
An `if let` guard in a match arm is unstable before 1.94, so write a plain guard and an `if let` inside the arm body; `let` chains in `if` and `while` are stable since 1.88.

## Scope

dssr is pull-only: it reaches each source only through the source's OAI-PMH endpoint, and every change stays inside this repository.

## Architecture

`dssr serve` is the single owner of the index, because the tantivy writer holds an exclusive lock file in `index_path/tantivy`.
Every subcommand except `update`, `reindex`, `revoke`, and `uninstall` is an HTTP client of the admin unix socket (`call` in `src/main.rs`), so it needs a running `serve`.
Members of the group `dssr` reach the socket without `sudo`, because `listen_unix` sets mode 0660 and the unit sets `RuntimeDirectoryMode=0750`.
`dssr update` runs `install.sh` from the tag of the latest release, or of `DSSR_VERSION` when the operator sets it, with `DSSR_VERSION` set, never from `main`, so the installer matches the release files.
`dssr reindex` stops the service, deletes the index directory, and starts the service, whose startup harvest then reads every record because the cursor went with the index.
`dssr revoke` writes the hash of a new `sk-` token for the client `default` to the configuration and shows the token before it restarts the service, which revokes the previous token.
`dssr uninstall` stops the service and removes the installation from the host.
These run as root and do not contact `serve`.

A harvest flows through `src/harvest.rs` (schedule), `src/oai.rs` (`ListRecords` paging), `src/record.rs` (`dim` or `oai_dc` whitelist mapping), and `src/index/` (tantivy documents); `src/server.rs` serves the HTTP API and the admin socket.
Each document stores the full record as JSON in the stored field `record`, and search results and record lookups return that JSON.
`index_path` holds `tantivy/` and `state.json`, which keeps the cursor, generation, and suspension of each source.
`serve` refuses an `index_path` that holds `index_meta.json`, the index of a release before 0.1.0, and asks for `sudo dssr reindex`, which recognizes that file too.
Releases ship only `linux/amd64`, as a static `x86_64-unknown-linux-musl` binary.

## Invariants

- The incremental `from` is the cursor minus 24 hours, because a DSpace datestamp is the change time, not the time the record reached the OAI core. It is sent as a date without a time, because SLiMS rejects a time in `from` although its `Identify` declares seconds, and OAI-PMH requires every repository to accept a date. The cursor advances only after a successful run.
- A full harvest is mark-and-sweep by generation. The sweep is skipped when the run fails or sees fewer than 90% of the indexed records, because DSpace answers `noRecordsMatch` when its OAI core is empty or Solr fails.
- tantivy stores the schema in `meta.json` and refuses to open an index with a different one, and it indexes with the analyzer of the release that wrote each document, so a change to `schema` in `src/index/mod.rs` or to the analyzer requires `sudo dssr reindex`, while a change to the `record` mapping requires `dssr harvest --full`.
- The whitelist keeps only the `dc` schema and drops `dc.description.provenance` (staff names and e-mail addresses) and every non-`dc` schema (may hold student numbers). `oai_dc` (the default prefix) has no qualifiers, so the whitelist cannot drop provenance from it and relies on the source crosswalk to leave it out, as the stock DSpace 6.3 `oai_dc.xsl` does. Every unqualified `dc:description` maps to `abstract`, because only `abstract` is searched. `dssr status` counts dropped fields, and that count is how the whitelist grows.
- Results carry the full record because clients pass them to AI agents as context.
- Search weights, fuzzy settings, and stemming change only on evidence from `dssr eval`.
- The scoring model in `src/index/node.rs` and `src/index/search.rs` is the one the weights were tuned with, so it changes only on evidence from `dssr eval`: BM25 with the square root of the term frequency, idf squared, and the average field length rounded up from the summed term document frequencies; a coordination factor on every disjunction; and fuzzy distance 1 with transpositions, each candidate weighted by 1/(1+distance). tantivy's own `TermQuery` and `FuzzyTermQuery` score differently, so search does not use them.
- The Indonesian stop list is the Lucene list in `src/index/stop_words_id.txt` minus `CONTENT_WORDS` (nouns, adjectives, time words, negation), because the stock list drops words such as `dini`, `diri`, and `kecil`.
- The whole-query title boost searches `title_phrase`, whose analyzer closes the gaps that stop words leave, so a quoted phrase on `title` stays exact.

## Adding a mapped field

A field is mapped when these five places agree:

1. the `Record` struct in `src/record.rs`,
2. the `add` match in the same file,
3. the fixture `testdata/record/dim.xml`, and `oai_dc.xml` when `oai_dc` carries the field,
4. the expected records in the tests of `src/record.rs`,
5. the `Record` schema in `src/openapi.json`.

## Release

Tags are bare SemVer (`0.2.0`), and only the GitHub release title carries the `v` (`v0.2.0`).
`scripts/package.sh` sets the version through `DSSR_BUILD_VERSION=<tag>` at build time, which `version` in `src/main.rs` reads.
Asset names carry no version because `install.sh` downloads from `releases/latest/download/<name>`.

`CHANGELOG.md` changes only in a release commit.
When the user asks for release `X.Y.Z`:

1. Read every commit since the previous tag with `git --no-pager log --oneline <previous-tag>..HEAD`, or the whole history when no tag exists.
2. Add `## [X.Y.Z] - YYYY-MM-DD` above the previous version section, with entries under the Keep a Changelog headings (Added, Changed, Deprecated, Removed, Fixed, Security) written for operators, not per commit. The step is done when every user-visible change since the previous tag has an entry.
3. Add the link reference `[X.Y.Z]: https://github.com/kuueri/dssr/compare/<previous-tag>...X.Y.Z`, or `https://github.com/kuueri/dssr/releases/tag/X.Y.Z` for the first release.
4. Hand the user these commands: `git add CHANGELOG.md`, `git commit -m "release: X.Y.Z"`, `git tag -s X.Y.Z -m X.Y.Z`, `git push origin main X.Y.Z`.

The release workflow publishes the new section, followed by GitHub's generated change list, as the release notes.
It fails when the tag is not a signed annotated tag that GitHub verifies, or when the section for the tag is missing or has no `- ` entries.
