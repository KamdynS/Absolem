# absolem — Design

> A precursor to code review: see the *shape* of a change before reading any code.
> The name nods to Alice's caterpillar — a hint about looking through the looking glass.

This doc names the *direction* and the *load-bearing distinctions*. Specifics —
module layout, exact IR types, milestone boundaries — sharpen as the code
exists to anchor them. Where this doc disagrees with running code, the code
usually wins; the doc is here to keep us pointed at the same horizon.

---

## 1. Purpose

`absolem` shows how a change moves a codebase's **structural surface** — the
types, interfaces, functions, methods, fields, variants, and consts that were
added, removed, or modified — so a reviewer walks into the diff already
holding a mental model of *what shape changed*. Bodies are not the point; the
contracts are. This compresses orientation, sharpens type-design review, and
turns body reading from open-ended comprehension into hypothesis verification.

## 2. The end goal — three frontends, one engine

At maturity, the same engine feeds three places a reviewer might want it:

1. **An interactive TUI** opened against a branch or MR before diving in.
2. **A Neovim plugin** that consumes the same IR and lets you navigate the MR
   with native go-to-definition / find-references — fixing the review
   workflow's "my editor was taken away from me" pain.
3. **A CI pipeline comment** — non-interactive markdown posted by the
   pipeline, reaching reviewers who never invoke a TUI.

They look like three products; they share a *symbol graph*. Unify at the
core, vary the chrome.

The current target is the smallest end-to-end thing: a syntactic TUI
showing what items changed in a Go MR. Everything beyond is direction.

## 3. The backend/frontend distinction

The shape everything else hangs off:

```
producer.extract(ref) ─► Surface ┐
                                  ├─► core.diff() ─► ChangeSet ─► frontend
producer.extract(ref) ─► Surface ┘
```

- **Producers** turn a git ref into a Surface (the structural API at that ref).
- **The core** diffs two Surfaces into a ChangeSet.
- **Frontends** display a ChangeSet.

Layers don't reach across this boundary. *That* is the design — not whichever
module structure or crate split happens to express it on a given day. The
project starts as one binary; parts will split into crates as a second
consumer of a layer appears (a Neovim plugin reading the IR over JSON; a
second producer). Splitting later is cheap; splitting prematurely bakes in
boundaries before usage has confirmed them.

**Extraction is not navigation.** Extraction (*what changed*) is offline,
deterministic, against two refs — no language server. Navigation
(*go-to-definition / find-references*) is live, against the working tree,
used by the TUI and Neovim frontends, backed by the user's own language
server. Don't conflate their lifetimes.

## 4. Non-goals

Deliberate. An agent that finds itself building one of these without explicit
instruction has drifted — stop and ask.

- **Not a correctness checker.** Shape, not behavior.
- **Not a linter or gate.** Informs; never fails builds.
- **Not a compiler.** Where real resolution is needed, lean on existing
  tooling (gopls, rust-analyzer, rustdoc JSON) — never reimplement a type
  checker.
- **Not a fleet of binaries.** One shipped binary. Languages enter via
  vendored tree-sitter grammars and the user's own language servers — never
  via a per-language analyzer we built or distribute.
- **Not tied to one forge.** GitLab and GitHub are both addressable; the
  core knows nothing about either.

## 5. Vocabulary

Worth picking the words now — they're cheap and reduce cross-doc drift.

- **Surface** — the structural API of a codebase at one git ref. One per ref.
- **Item** — a single API entity: struct, interface, function, method, field, …
- **ItemId** — stable identity (`path + kind + disambiguator`). Cross-ref
  matching depends entirely on this.
- **TypeRef** — a reference from one item to a type it uses. May be
  *resolved* (points to an `ItemId`) or merely *displayed*, per tier.
- **ChangeSet** — the diff of two Surfaces.
- **Producer** — emits a Surface for a ref.
- **Tier** — a Surface's fidelity: **Syntactic** (tree-sitter, no resolution)
  or **Semantic** (real resolution via the language's own tooling).
- **Frontend** — a consumer of a ChangeSet: TUI, Neovim plugin, pipeline comment.

## 6. One binary, consume don't ship

`absolem` is one shipped binary, and it never builds or distributes a
per-language analyzer. Language data comes from things that either compile
into our binary or the user already has. In order of preference:

1. **In-process tree-sitter (Tier 0 — Syntactic).** Grammars are C, vendored
   in. Adding a language is a `.scm` query file — data, not code. The
   breadth mechanism.
2. **LSP against the user's own language server (Tier 1 — Semantic).** gopls,
   rust-analyzer, tsserver. Drives navigation; can enrich extraction.
3. **CLI tooling the user already has (Tier 2 — Semantic).** Where a
   language exposes its semantics via a CLI emitting structured JSON
   (rustdoc is the archetype), invoke it and parse — don't reimplement,
   don't ship.
4. **Out-of-process native adapter — escape hatch.** Discouraged; needs
   explicit sign-off.

## 7. Direction

Roughly, in this order — the *order* is a current best guess, not law. What
matters: the smallest end-to-end thing that runs on a real diff, then grow
from real usage.

Landed so far:

- The syntactic TUI, for Go **and** Rust (second language arrived early):
  tree-sitter producers behind a registry, member-level extraction
  (fields, variants, interface/trait methods), core diff, CLI wiring
  with arbitrary `base..head` ranges.
- Three more frontends over the same ChangeSet: plain text, JSON
  (schema-versioned — the seam the Neovim plugin will consume), and the
  markdown CI pipeline comment.
- A first slice of navigation: the TUI jumps to an item in `$EDITOR` at
  its head-side line. Editor launch is a capability; no language server
  yet.
- Displayed-tier `TypeRef`s: producers collect the type names each
  signature mentions; a head-wide index resolves them by name, and the
  TUI expands a row's referenced types inline. Semantic tiers upgrade
  the same edges to real resolution.
- Member-level grouping with context: a changed composite renders whole,
  unchanged members dimmed, so the diff reads like the type it changed.

- The reference graph is navigable: expansion rows are first-class
  (identity-keyed, recursive, cycle-guarded, jumpable), `gr` inverts the
  edge to answer *what uses this type*, and name resolution keeps every
  colliding definition, preferring the referencing file and reporting
  the contest instead of guessing silently.

Next:

- Go goes semantic via gopls: resolved `TypeRef` edges, real
  jump-to-definition / find-references from the TUI.
- The Neovim plugin consuming the JSON IR.
- Wire the markdown frontend into an actual CI pipeline.

If something forces the producer / core / frontend boundary to leak, or
forces a second shipped binary, treat that as a signal to stop and rethink —
not a license to accept it.

## 8. Document map

- [`DESIGN.md`](./DESIGN.md) — this file. Direction and load-bearing distinctions.
- [`STYLE.md`](./STYLE.md) — Rust style and the capability-injection rule.
- [`AGENTS.md`](./AGENTS.md) — how to work in this repo: cadence, MR hygiene, DoD.
