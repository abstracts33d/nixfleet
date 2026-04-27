# `nixfleet_docgen::summary`

Generate `SUMMARY.md` for the mdbook by walking the `manual/`
and `generated/` subtrees.

Both subtrees use the same convention: any `.md` file is a
chapter; directories with the same name as a `.md` file at the
same level become its sub-pages (mdbook's nested layout).

Output is sorted alphabetically per directory so reruns are
byte-identical. Manual content is emitted before generated
content — the curated narrative leads, the auto-extracted
reference follows.

## Items

### 🔓 `fn run`

_(no doc comment)_


### 🔒 `fn format_relative_link`

Compose the path that mdbook expects: relative to `src/`. Walk
ancestors until we hit the `manual` or `generated` segment,
then prepend that segment.


