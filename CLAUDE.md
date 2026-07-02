# CLAUDE.md

## Project

`absolem` is a precursor to code review: it surfaces the *shape* of a change
— the types, interfaces, functions, methods, fields that moved — before the
reviewer reads any bodies. A nod to Alice's caterpillar; a hint about
looking through the looking glass.

**The end goal — three frontends, one engine.** At maturity, the same
engine feeds an interactive TUI, a Neovim plugin (review with native
go-to-def / find-refs), and a CI pipeline comment. They look like three
products; they share a *symbol graph*. Unify at the core, vary the chrome.

**The current state**: the syntactic tier is end-to-end for Go and Rust —
member-level extraction (fields, variants, interface/trait methods), four
frontends (TUI, plain, JSON, markdown), arbitrary ref ranges, and a TUI
jump to `$EDITOR`. **The current target** is the semantic tier: Go via
gopls (resolved TypeRefs, real navigation). Everything beyond is
direction, not specification — the project morphs as code lands.

## Backend/frontend — the load-bearing distinction

```
producer.extract(ref) ─► Surface ┐
                                  ├─► core.diff() ─► ChangeSet ─► frontend
producer.extract(ref) ─► Surface ┘
```

- **Producers** turn a git ref into a Surface.
- **The core** diffs two Surfaces into a ChangeSet.
- **Frontends** display a ChangeSet.

Layers don't reach across this boundary. *That* is the design — not
whichever module structure or crate split happens to express it on a given
day. Starts as one binary; parts split into crates as second consumers of
the IR appear.

**Extraction is not navigation.** Extraction (*what changed*) is offline,
deterministic, against two refs. Navigation (*go-to-def / find-refs*) is
live, against the working tree, backed by the user's language server.
Don't conflate their lifetimes.

## Non-goals — never build these unprompted

- **Not a correctness checker.** Shape, not behavior.
- **Not a linter or gate.** Informs; never fails builds.
- **Not a compiler.** Lean on existing tooling (gopls, rust-analyzer,
  rustdoc JSON) — never reimplement a type checker.
- **Not a fleet of binaries.** One shipped binary. Languages enter via
  vendored tree-sitter grammars and the user's own language servers —
  never via a per-language analyzer we built.
- **Not tied to one forge.** GitLab + GitHub both addressable; core knows
  nothing about either.

## Vocabulary — use these exactly

- **Surface** — the structural API at one git ref.
- **Item** — a single API entity: struct, interface, function, method, …
- **ItemId** — stable identity (`path + kind + disambiguator`).
- **TypeRef** — a reference from one item to a type it uses. May be
  *resolved* or merely *displayed*, per tier.
- **ChangeSet** — the diff of two Surfaces.
- **Producer** — emits a Surface for a ref.
- **Tier** — fidelity: **Syntactic** (tree-sitter) or **Semantic** (real
  resolution).
- **Frontend** — a consumer of a ChangeSet.

## One binary, consume don't ship

Language data comes from things that either compile into our binary or the
user already has, in this order of preference: in-process tree-sitter
(Tier 0) → user's LSP (Tier 1) → user's CLI tooling emitting JSON like
rustdoc (Tier 2) → out-of-process adapter (escape hatch, sign-off
required).

## Working model

The user is PM / tech lead. Agents implement features as single-focus MRs;
the user reviews and steers. This is **not** a vibe-coded repo.

- **One MR per feature.** Reviewable in one sitting (a few hundred lines,
  not thousands).
- **One MR at a time.** Open it, hand it off, wait. Do not stack or queue.
- **Plan before non-trivial work.** Post an ordered list of the MRs you
  intend to open; wait for approval before coding.
- **Walking skeleton first.** Thinnest end-to-end slice that runs, then
  add depth.
- **Commit history is part of the deliverable.** Imperative subject; no
  `wip` / `fix typo` / `address review` noise — squash or rewrite before
  opening.
- **MR descriptions** state *what* changed, *which boundary or rule* it
  respected, and *how* it was verified.

## Code rule that is binding from day one

**Capability injection.** Pure code (the IR, the diff engine) takes no
ambient effects. A function that needs IO, time, a subprocess, or network
takes a capability parameter (`impl ProcessRunner`, `impl Clock`,
`impl FileSystem`, …). Only the composition root constructs real
capabilities. Tests use in-memory fakes — a test that needs a temp dir or
subprocess means a capability wasn't injected.

The lint baseline in `Cargo.toml` backs this; deeper context in
[`docs/STYLE.md`](./docs/STYLE.md).

## Communication style

Be succinct. Most questions are short — answer them that way. Only expound
when the topic warrants it (subtle tradeoffs, gotchas, design forks). Skip
framing, recaps, and closing summaries. If unsure whether to expand, err
short — the user will ask for more.

## Deeper docs

- [`docs/DESIGN.md`](./docs/DESIGN.md) — direction and load-bearing distinctions.
- [`docs/STYLE.md`](./docs/STYLE.md) — capability rule, error handling, dependency hygiene.
- [`docs/AGENTS.md`](./docs/AGENTS.md) — boundaries, cadence, DoD, MR/commit hygiene.
