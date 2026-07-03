# absolem — Agent Operating Manual

> For any AI agent (and any human) doing work in this repo. Read it at the
> start of every session. It governs **process**. [`DESIGN.md`](./DESIGN.md)
> governs architecture and [`STYLE.md`](./STYLE.md) governs code; on their
> topics, they win. This doc tells you how to work without drifting.

---

## 0. Prime directive

Build the **smallest correct change** that satisfies the task, without
crossing an architectural boundary or introducing an ambient effect. When a
task tempts you outside those lines, **stop and ask** — do not improvise
around the rules.

Deliver that change the way a careful human would: **one small,
self-contained MR at a time, then stop and wait for review.** Never attempt
several MRs' worth of work in a single shot. The human is reviewing as code
arrives; outrunning that review defeats the entire point. Cadence rules
are in §6.

## 1. Before you touch code

1. Read [`DESIGN.md`](./DESIGN.md) (§3 backend/frontend, §4 non-goals, §6
   sourcing order, §7 direction) and [`STYLE.md`](./STYLE.md) (§3
   capabilities, §4 errors).
2. Identify what the **current target** is from `DESIGN.md` §7. If unclear,
   ask — do not guess and build ahead.
3. Restate the task in one sentence and name which layer it touches
   (producer / core / frontend / CLI). If it touches more than one layer,
   it's probably too big — split or ask.

## 2. Hard boundaries — never cross these

Each line names the rule, what drift looks like, what to do instead.

- **The producer/core/frontend boundary (DESIGN §3).** Producers make a
  `Surface`; the core diffs Surfaces into a `ChangeSet`; frontends display
  a `ChangeSet`. *Drift:* a frontend reaching into language specifics, or
  the core importing tree-sitter. *Instead:* move the logic to the
  correct layer.
- **One binary, consume don't ship (DESIGN §4, §6).** *Drift:* adding a
  build step for a per-language analyzer, reimplementing a type checker,
  or shipping a second binary. *Instead:* tree-sitter (vendored), the
  user's existing tooling, or LSP. Out-of-process adapters need explicit
  human sign-off.
- **Capability injection (STYLE §3).** *Drift:* calling `std::fs`,
  `SystemTime::now`, `std::process::Command`, or any network/IO directly
  inside pure code. *Instead:* take a capability parameter; construct real
  effects only at the composition root.
- **The non-goals (DESIGN §4).** Not a linter / gate, not a replacement
  for review, not a compiler, not a fleet of binaries. *Drift:* building
  any of these unprompted. *Instead:* stop and ask whether it's actually
  wanted.

## 3. Scope discipline

- **Direction is loose, but it has an order.** The current target lives in
  `DESIGN.md` §7. Don't pre-build later targets — even if it seems easy.
  A half-built later target is worse than none.
- **No gold-plating.** No speculative generality, no config knobs nobody
  asked for, no "while I'm here" refactors riding a feature change.
- **The IR types, once they exist, are a design event to change.** Stop,
  state the case, get human sign-off before editing them. Most tasks
  should not touch them.

## 4. How to work

- One layer, one concern, one MR. Keep changes reviewable.
- Test against **fake capabilities**, not real IO. If a test needs a temp
  dir, a real clock, a subprocess, or the network, a capability wasn't
  injected — fix the design, not the test (STYLE §3.5). The one
  exception is the end-to-end suite in `tests/`, which exists precisely
  to validate the real capability implementations against real git.
- Use project vocabulary exactly (`Surface`, `Item`, `ItemId`, `TypeRef`,
  `ChangeSet`, `Producer`, `Tier`, `Frontend`). No synonyms.
- Adding a dependency is a design event (STYLE §8). Justify it, or don't
  add it.
- Assume no network at build time beyond what's already vendored /
  declared.

## 5. Definition of done

A change is not done until **all** of these hold:

- [ ] `cargo fmt -- --check` is clean.
- [ ] `cargo clippy --all-targets -- -D warnings` is clean.
- [ ] `cargo test` passes.
- [ ] No new ambient effect in pure code; no new `unwrap` / `expect` /
      `panic` in library paths.
- [ ] No producer/core/frontend boundary violation; no new shipped binary.
- [ ] If behavior or direction changed, the relevant doc was updated in the
      same change. Code and docs do not drift apart.

Don't report success on unverified work; if you couldn't run a check, say
so explicitly.

## 6. Delivery cadence — develop like a human

The default failure mode of an AI here is doing everything at once. Don't.

- **One logical change per MR.** Reviewable in a single sitting — target a
  few hundred lines of diff, not thousands. If it's growing past that,
  split.
- **One MR at a time.** Open it, hand it off, and **stop**. Do not start,
  stack, or queue the next MR until this one merges or you're told to
  proceed.
- **Plan before you code anything non-trivial.** For work larger than one
  obvious step, post a short ordered list of the MRs you intend to open
  and wait for the human to approve the plan. Don't write code against an
  unapproved breakdown.
- **State intent before each increment.** One sentence — *"next MR adds
  the ItemId matcher to the diff engine"* — so the human can redirect
  before effort is spent.
- **Walking skeleton first.** Build the thinnest end-to-end slice that
  compiles and runs, then add depth in later MRs. Derisk the design
  early.
- **Every merged MR leaves `main` green:** it compiles, tests pass, it
  stands on its own. No "this half works, the rest is coming."
- **Never bundle unrelated changes.** Spot something else worth doing?
  Note it for a separate MR.

### Commit & MR hygiene (the human is particular about this)

- **Commits within an MR tell a logical story.** Imperative subject. No
  `wip` / `fix typo` / `address review` noise — squash or rewrite before
  the MR is opened.
- **The MR description states:** *what* changed, *which boundary or rule*
  it respected, and *how* it was verified (DoD §5 commands run + relevant
  output). A reviewer should never have to reverse-engineer your intent.
- **Link the MR to its plan / approval.** If the human approved an ordered
  list of MRs in §6, the MR description names which entry it implements.

## 7. Stop and ask when…

- The task tempts you across a §2 boundary or away from the current
  target.
- It would change the IR types (once they exist), add a dependency, add a
  binary, or restructure the layout.
- It would require an out-of-process adapter or reimplementing language
  tooling.
- Requirements are ambiguous in a way that changes the design. One good
  question beats a confident wrong guess.
