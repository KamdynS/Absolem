# absolem

[![CI](https://github.com/KamdynS/Absolem/actions/workflows/ci.yml/badge.svg)](https://github.com/KamdynS/Absolem/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

> See the *shape* of a change before reading any code.

`absolem` is a precursor to code review: it surfaces the structural API
that moved — types, interfaces, functions, methods, fields, variants,
consts — between two git refs, so you walk into a diff already holding a
mental model of what changed. Bodies are not the point; the contracts
are. The name nods to Alice's caterpillar.

## Usage

```sh
absolem                    # review origin/main...HEAD in the TUI
absolem main..v2.0         # any range; .. and ... keep git semantics
absolem release            # bare base, head defaults to HEAD
absolem --worktree         # your uncommitted work, untracked files included
absolem HEAD.. -w          # just the uncommitted delta
absolem --pr 42            # fetch and review a PR/MR from origin by number
absolem --plain            # plain text (also the default when piped)
absolem --json             # schema-versioned JSON for machine consumers
absolem --markdown         # forge-flavored markdown for a CI comment
```

A bare base and `...` compare against the merge base — what GitHub and
GitLab show on a PR. `..` diffs the two trees directly. Runs from any
directory inside the repo; without a remote it falls back to your local
`main`/`master` as the base.

Output marks each item `+` added, `-` removed, `~` modified (with the
old signature beneath). A changed composite renders **whole** — every
field, variant, or method listed, unchanged members as dimmed context,
the changes in place. Files whose API shape did not move are omitted; a
body-only change reports exactly that: no structural changes.

### The TUI

Vim-style: `j`/`k` move (counts work: `5j`), `{`/`}` hop files,
`Ctrl-d`/`Ctrl-u` scroll, `gg`/`G` ends, `H`/`M`/`L` and `zt`/`zz`/`zb`
position, `/` search with `n`/`N`, `q` quits. Colors are ANSI palette
entries, so the view takes on your terminal's theme; modified rows mark
exactly the tokens that changed.

`Tab` expands the types the current row references — one level per
press: first the definition header, another `Tab` on it for its
members, and so on down (cycles are guarded), resolved by name against
the whole tree at head. Unfolded rows are first-class: cursor onto
them, `Enter` to open one at its declaration. `gr` shows references —
every item whose signature mentions the current type unfolds as a
jumpable list. When a name has competing definitions the header says so
("1 of 3 definitions") rather than guessing silently. `Enter` opens the item
at its line in `$VISUAL`/`$EDITOR` (strictly those; if neither is set,
absolem tells you rather than guessing an editor).

## Languages

Go and Rust, at the **syntactic tier**: in-process tree-sitter grammars,
no resolution, deterministic against two refs. Extraction is
member-level — a struct field, enum variant, or interface method change
diffs to exactly that member. Semantic tiers (gopls, rust-analyzer)
are direction, not yet code; see [`docs/DESIGN.md`](docs/DESIGN.md).

## Build

```sh
cargo build --release     # one binary; grammars are vendored in
```

## Architecture

```
producer.extract(ref) ─► Surface ┐
                                  ├─► core.diff() ─► ChangeSet ─► frontend
producer.extract(ref) ─► Surface ┘
```

Producers turn a ref into a `Surface`; the core diffs Surfaces into a
`ChangeSet`; frontends (TUI, plain text, JSON, markdown) display it.
Deeper reading: [`docs/DESIGN.md`](docs/DESIGN.md),
[`docs/STYLE.md`](docs/STYLE.md), [`docs/AGENTS.md`](docs/AGENTS.md).
