# law-smith: write the book for the work in front of you (RFC 0018)

The **book** is a set of deterministic laws that judge what a team makes: code, copy, reports, plans, tables, web pages, images. You write laws as data; the engine runs them with no model; the person ratifies them. A team that has a `law-gate` step is judged by the ratified laws and a failed gate sends the findings (a file, a line, a number, a fix) back to the agent that must repair.

## The loop

1. **Name the work.** What kinds of artifact will this team produce (`markdown`, `text`, `html`, `json`, `yaml`, `csv`, `image`, `svg`, `code`)? What does "good" mean for each, in checkable terms (a length, a banned word, a required section, a field that must exist, a size)? If a success criterion cannot be checked by code, do not write a law for it: say so to the person.
2. **Look first.** `antiloki_book op=list` shows the shipped packs (technical-docs, marketing-copy, seo-report, project-plan, data-csv, html-page, brand-images). Reuse before writing. `op=show pack=<id> law=<key>` shows one law with its examples.
3. **Write a pack** (YAML or JSON): `id`, `kinds`, optional `paths`/`exclude` globs, and `laws`. Each law: `key` (kebab), `title`, `detector`, `config`, `severity` (info/warn/error/critical), `weight` (0-10), `axis`, `fix` (one sentence), and **`fixtures`**: at least one `pass` and one `fail` example (`text`, or `b64` for an image). A law without both cannot be ratified.
4. **Test.** `op=test law={...}` runs the law against its own examples. Fix the law or the example until they agree. Then `op=propose pack=<the pack>` saves it as **candidates** (they advise, they never gate).
5. **Show the hit-rate.** `op=run candidates=true` on the real files: how many fire? A law that fires on most of the good files is too strict; one that never fires is dead. Tell the person plainly.
6. **Ask for ratification.** Only the person ratifies, in the app. Never claim a candidate gates.

## Detectors (all deterministic)

| detector | config | finds |
|---|---|---|
| `pattern` | `unit` (file, line, paragraph, sentence, heading, item, cell…), `regex` or `any: [words]`, `mode: forbid|require`, `ignore_case`, `whole_word` | a forbidden or missing text |
| `count` | `unit`, `attr`, `min`, `max` | a number out of range (`words`, `sentences`, `avg_sentence_words`, `title_len`, `width`, `megapixels`, `rows`, `empty_cells`…) |
| `structure` | `check`: `parses`, `heading_levels`, `single_h1`, `no_empty_sections`, `unique_headings`, `required_sections {names}`, `section_order {names}`, `alt_text`, `csv_rectangular`, `no_duplicate_rows`, `unique_header`, `fence_language` | the shape of a document or table |
| `schema` | `required`, `required_each {array: [keys]}`, `forbidden`, `types`, `enums`, `min_items`, `unique`, `refs [{from,to}]` (paths like `tasks[].id`) | a structured file (YAML, JSON, TOML) that breaks its own rules |
| `link` | `images` | internal links, anchors and images that point at nothing |
| `similarity` | `min_words`, `max_distance`, `across` | the same paragraph twice, in a file or across files |
| `expr` | `unit`, `expr` (a small boolean expression over the unit's attributes: `exclamations > 2`, `!has_lang`, `aspect > 2.5`), `message` | anything else you can say in one line; no loops, no I/O |
| `cross` | `for` (glob), `needs` (`{dir}`, `{stem}`, `{name}`) | a file that needs a sibling that is not there |

## Attributes you can use (`count` and `expr`)

Every text file: `words`, `chars`, `lines`, `paragraphs`, `sentences`, `headings`, `h1_count`, `links`, `images`, `images_without_alt`, `todo_count`, `placeholder_count`, `exclamations`, `caps_words`, `avg_sentence_words`, `avg_word_len`, `flesch_en` (English only), `reading_minutes`. Paragraph and item units: `words`, `sentences`, `longest_sentence`. HTML: `title_len`, `has_meta_description`, `meta_description_len`, `has_lang`, `inline_styles`. CSV: `rows`, `cols`, `empty_cells`, `ragged_rows`, `duplicate_rows`. Images: `width`, `height`, `aspect`, `megapixels`, `bytes`, `format`, `has_alpha`. SVG: `has_script`, `has_title`, `elements`.

## Rules of the craft

- **Deterministic or not a law.** If you cannot state it so that the same file always gives the same answer, it is advice for a person, not a law.
- **Say where.** Prefer units (paragraph, heading, row) so a finding has a place.
- **One idea per law**, a `fix` the agent can act on, a severity that matches the damage (`error` fails the default gate; `warn` lowers the score).
- **Scope it** with `kinds` and `paths` so a README's laws do not judge a landing page.
- **Never lower a gate to make a run pass.** Fix the work, or tell the person the law is wrong.

## Docs rules: documentation that fits the project (`purpose: doc`)

The generated documentation of a file (the docs icon on a file in the Editor) is made by the book too, so it fits the kind of project the team produces, not only code. A law with `purpose: doc` **documents instead of judging**: when its detector finds something, the file's documentation gets a row in the section the law names. It never becomes a finding, never scores, never gates.

- `config.section`: the section title (`Design patterns`, `HTTP routes`, `Mechanics`, `Calls to action`, `Characters`…). `config.label`: what a hit means (`Observer`, `A route`, `A combo`).
- Use the same detectors as checks: `pattern` (the usual one: a regex or `any: [words]` over a unit), `count`, `structure`, `expr`, `schema`. A hit shows the first lines where it was found.
- Same lifecycle as any law: examples (a file with nothing to document, a file with something), test, propose as a candidate (its rows say "proposed, not ratified"), the person ratifies.
- The base of every file's documentation (what it is, its outline, its definitions, its columns, its imports) is built in and needs no law. Shipped: `code-docs` (design patterns, routes, configuration, what a file touches) and `copy-docs` (numbers and claims, calls to action, questions, contact points). For a games studio you write `Mechanics`; for a legal team `Obligations`; for a data team `Sensitive columns`.
