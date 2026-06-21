# Type-Skeleton Review Tool — Design Doc

*Working name: TBD (e.g. `skel`, `skim`, `typediff`). A tool that surfaces the type skeleton of a git diff as a grounding pass before reading the actual changes.*

---

## 1. The problem

AI has increased the volume of code I review. In Rust, the type signatures carry most of the semantic weight of a patch — a changed `fn` signature, a new enum variant, an error type that shifted from `Result<T, E>` to a different `E`, a new trait impl. The bodies are often just the mechanical realization of the type-level contract. When I review, I go to the types first; they're a high-signal, low-volume compression of the change.

I want a tool (TUI first) that reads `git` + type information and shows me the **type skeleton of a diff** — the signatures of what changed, with the bodies stripped — as a quick precursor to reading the real diff. A way to ground myself in 5 seconds before I read business logic and control flow.

## 2. What this actually is (the sharpened value)

The pitch started as "orient faster," but that's the weakest framing and the one most prone to false confidence. Under scrutiny it became three stronger things:

1. **A type-design review surface.** With bodies stripped, you're looking at the data model and the interface contracts *as a thing in themselves* — are the nouns right, are the error types honest, is this function promising too much or too little. Bodies actively get in the way of this kind of review; removing them is the point. Nobody mistakes "I evaluated the type design" for "I verified the logic."

2. **A priming tool for body-reading.** The skeleton loads a sharp, per-function expectation ("this should only set a name"). Then reading the body becomes a *test of that hypothesis* rather than open-ended comprehension. A side effect that violates the contract — analytics fired inside a `set_name` — screams, where in a cold read it would just look like "stuff the function does." This turns body-review from comprehension into verification, which is faster and catches more.

3. **A forcing function on type design.** Making type design legible *at review time* creates pressure toward good type design. If the reviewer's first move is the skeleton view, people start writing code that reads well in the skeleton: more newtypes, sharper error types, names that carry weight, explicit dependencies. What gets surfaced gets improved. The viewer and the incentive are the same artifact. "Only works if the code is good" quietly becomes "also nudges the code toward good."

## 3. What this is NOT (honest limitations)

- **Not a correctness checker.** Code that type-checks but is subtly wrong — off-by-one, a flipped condition, a panicking `unwrap`, a silently swallowed error — is invisible at the type level. This is *exactly* the dangerous failure mode of AI-generated code, and the skeleton can give false confidence on the patches that most deserve suspicion. This is a "review type design / review faster" tool, not a "catch the bad AI code" tool — unless deliberately extended into a risk surface (see roadmap).

- **It primes, it doesn't detect, the "clean signature / fat body" case.** A function that *looks* clean but secretly does three extra things has, by construction, a signature that hides them. The skeleton won't stop you there; the **body read** stops you. The skeleton's job in that case is to make the deviation salient, not to flag it.
  - **Caveat that recovers a lot of this:** in a codebase with no ambient/global state, the **parameters are an honest declaration of reach**. A function handed only `&mut self` literally cannot touch a database. So a fat body can't hide — it would need params it doesn't have, and those show in the skeleton. The skeleton catches case-2 reach *to the degree the codebase avoids hidden state.*

- **Two flavors of bad API; it only catches one cleanly.** *Bad types* (stringly-typed, god `Error` enum, primitive obsession, `do_thing` names) jump out hard in the skeleton — arguably harder than in the diff, because the body normally camouflages them. *Bad design wearing clean types* is invisible, and a clean skeleton can even hand you a false "this design is good." **High precision, imperfect recall** on "is this well designed": if it looks bad in the skeleton it's bad, but looking good doesn't clear it.

- **Understanding vs. flagging.** On *bad* code the tool is useless for comprehension (no good structure to lean on) but good for detection (the absence of structure is itself the signal). It won't help you understand a mess; it'll help you see that it's one.

## 4. The precondition that makes it load-bearing

The tool's value is roughly the product of two dials:

- **Expressiveness** — how much a signature can *say* (ADTs, generics, errors-in-the-type). Sets the ceiling on design-review value.
- **Honesty** — whether a clean signature actually *bounds* what the code does (no ambient state, explicit deps, visible effects). Decides whether a tidy signature is a promise or a lie.

Our codebase qualifies: clean CRUD, storage behind interfaces, side effects abstracted, good type design, explicit-ish dependencies. On code like this the skeleton is close to an honest map rather than a pretty lie. That's the ballgame for whether the tool works at all.

## 5. Technical approach

### Extraction — three paths

1. **`syn` (recommended MVP).** The mature, stable crate the macro ecosystem uses to parse Rust into an AST. Parse the changed files, walk `fn`/`struct`/`enum`/`impl` items, pull signatures, map them against the diff's changed line ranges to find touched items. Stable API, no process to manage, no cold start, weekend-sized.
   - *Ceiling:* purely **syntactic**. Sees `-> Result<Bar, Error>` as text; does **not** resolve what `Error` is, infer `let` bindings, or know trait impls. Fine for "declared signatures of changed items," useless for inferred types.

2. **rust-analyzer over LSP.** The `rust-analyzer` binary speaks standard LSP over JSON-RPC. Relevant methods: `textDocument/documentSymbol` (hierarchical symbol tree with kinds + a `detail` field carrying the signature), `hover` (resolved type/signature at a position), `inlayHint` (the **inferred** types `syn` can't give you), `callHierarchy` (the edges / call graph). Stable interface, real semantic resolution. Cost: async ceremony, managing a server process, and **cold-start latency**.

3. **`ra_ap_*` crates (max power, unstable).** rust-analyzer publishes its internals to crates.io: `ra_ap_syntax` (a genuine lossless syntax tree on `rowan`, walkable token-by-token), `ra_ap_hir` (semantic model: resolved types, trait resolution). Maximum power, but explicitly unstable, churns every release, barely documented. You'd be building on RA's guts and reading source to do it.

**Recommendation:** start with `syn`. Declared signatures get ~80% of the value. Graduate to LSP the moment you hit something inference-shaped you can't live without. Don't touch the internal crates until LSP demonstrably can't give you what you want.

### Two-revision problem

A true *before → after* type diff means analyzing both the old and new state (a git worktree for the old rev, an analyzer pointed at each). Doable, but it's the hard part. **The MVP sidesteps it entirely** by only analyzing the *new* state of touched items.

### Latency is existential

A "quick precursor" that takes 30 seconds dies. If/when RA is involved, a warm persistent instance and aggressive caching aren't polish — they're the product. `syn` avoids this problem entirely, which is another reason to start there.

## 6. Type closure — including referenced-but-unchanged types

The diff gives you a set of *changed* items, but a changed signature mentions types — `fn create_user(req: CreateUserRequest) -> Result<User, UserError>`. If `CreateUserRequest` / `User` / `UserError` aren't themselves in the diff, the skeleton shows opaque names and the grounding value collapses ("okay, but what's *in* `CreateUserRequest`? what variants does `UserError` have?"). So the tool's real input isn't the changed lines — it's the **transitive type closure** of the diff: the changed items, plus the definitions of the types they reference, recursively, bounded.

**The data structure is a type-dependency tree, not an AST** — worth stating, because it's an easy conflation (an AST is the syntax of a single file; this is a semantic graph spanning files). Level 1 is the *changed items* — types **and** functions, whose param/return/error types seed the tree — and their referenced types are expandable children. `syn` gives you a node's own shape and the *names* it mentions for free; the **edge** to where a mentioned type is defined, the thing that lets you expand a node, is name resolution, which no AST provides (not `syn`'s, not even rust-analyzer's syntax tree — that lives in RA's semantic/HIR layer or an LSP `definition`/`hover` call). And because shared and recursive types (`Box<Self>`, mutual recursion) make it a graph rather than a strict tree, lazy expansion needs cycle-breaking / "already shown" markers or it loops forever.

Three problems fall out of this:

- **Boundary (how deep / how to not pull in the whole crate).** Expanding referenced types to full depth pulls in the world (`User` → `UserId` → `Uuid` → …). Use a **leaf-set**: treat std and third-party types as opaque terminals (the reviewer already knows `Result`, `Uuid`); only expand **workspace** types. In a TUI the elegant version is **lazy expansion** — render the changed items plus their direct references, and let the reviewer drill deeper on a keypress. That turns "how deep?" into an on-demand question instead of a precomputed one.

- **Marking (context vs. change).** A pulled-in type is *context*, not a change, and must be visually distinct — dimmed or folded by default — or you've muddied the very diff signal the tool exists to sharpen. A node's mark ("changed" / "context") comes from whether it's in the diff, independent of how the closure reached it. The two interleave: an unchanged `UserError` can hold a variant whose payload type *did* change.

- **Resolution (the architectural consequence).** This is the important one. `syn` sees the *text* `CreateUserRequest` but has no idea where it's **defined** — that's name resolution (through `use` statements, the module tree, re-exports, aliases, generics), and `syn` doesn't do it. So **type closure is the concrete feature that pushes you from `syn` to rust-analyzer**, earlier and more centrally than the inferred-`let`-types example in §5. RA's `textDocument/definition` / `typeDefinition` / `hover` resolve exactly this for free.
  - *Honest stopgap:* on a constrained codebase (single crate, no glob imports, conventional layout, no macro-generated types) a crude "grep the workspace for the `struct`/`enum`/`type`/`trait` declaration matching this name" resolver gets surprisingly far — our clean-CRUD codebase may tolerate it for a while. But globs, re-exports, and aliases will make it wrong in annoying ways. The principled answer is to let RA resolve rather than reinvent the hardest part of a compiler frontend badly. Don't sink weeks into a half-correct resolver.

**Consequence for the build plan:** the pure-`syn`, changed-signatures-only rungs (§7, steps 1–2) are still the right *first* slices — they prove diff-mapping and signature extraction. But closure (step 3) is what actually makes the skeleton ground you, and it's the thing that forces the resolver/RA decision. Expect to confront it almost immediately, and decide deliberately between the crude resolver stopgap and biting the RA bullet — rather than discovering the wall by surprise.

## 7. Build plan — a walking skeleton

Principle: **thin vertical slices.** Every step ends with something that *runs and does a visible thing*. Never go dark for weeks; never a nothing-then-all-at-once build. Two design rules make this possible:

- **Separate the data logic from the rendering.** The valuable part (diff → touched items → closure) is logic; the TUI is presentation with a learning curve. Build the logic as a plain stdout CLI first so the TUI is never on the critical path to a useful tool. *A tree is just indentation* — you do not need a TUI to have one.
- **Put resolution behind a trait** (`trait Resolver { fn resolve(&self, name) -> Option<Def>; }`) from the moment closure exists. Then swapping the crude resolver for rust-analyzer is a second implementation built on a branch while the working tool keeps running — the one lumpy step never takes the project dark.

The ladder:

1. **Diff-parser CLI** — prints changed files + changed line ranges. Boring, but runs on day one or two. Concrete.
2. **Signature extractor CLI** — `syn`-parse the changed files, print the *touched* items' signatures to stdout. **First genuinely useful artifact.** Run it on a real patch and get partial grounding. No TUI.
3. **Closure as indented text (still CLI)** — crude `Resolver` pulls in referenced workspace types, printed nested under each changed item, context dimmed/marked. **Now it actually grounds you.** Still stdout, still no TUI.
4. **TUI** — feed the *same data* into `ratatui`: collapsible nodes, lazy expand, navigation. A presentation upgrade of something already working — you learn ratatui on correct data, not while fighting the logic.
5. **RA swap** — second `Resolver` impl backed by LSP `definition`/`hover`. Internal upgrade behind the same interface; the tool just gets more correct.

Honest caveat: step 2 is useful-but-*partial* — orientation on small, self-contained patches, capped until closure lands in step 3 (see §6). Don't be disappointed when it feels thin; that's expected.

Feedback loop: because steps 1–3 are stdout CLIs, **snapshot-test them** (`insta`) — run on fixture diffs, assert the printed output. Tight, verifiable progress every day, and it sidesteps the slow debug cycle a TUI or RA imposes. Something concrete *and correct* daily, which is the stronger version of the goal.

Keep it brutal at each rung: ship the smallest thing that runs, use it, and let actual usage decide whether the next rung earns its place.

## 8. Roadmap (beyond the ladder)

- **Call graph / edges** among the changed functions. The skeleton shows nouns and verbs but not which verb calls which; the edges are where "is this composed sanely" lives.
- **Risk surface** (the opinionated, AI-bug-catching extension): new `unsafe`, added `unwrap`/`expect`, error type changed or widened, new `Send`/`Sync`/`Drop`/`Deref` impls, visibility widened to `pub`, numeric casts, lifetime changes. Type-adjacent signals that correlate with "look here."
- **Before/after type diff** — the semantic signature delta across the patch (needs the two-revision setup).
- **Pipeline step / MR comment** — see distribution note below.
- **"Surface conclusions, not just signatures" mode** — "this error type got wider," "this function's reach grew," "these three signatures are vague." This is what lets the tool reach people who don't already have the type-first habit.

## 9. Distribution / survival

The TUI is the easiest thing to build and the easiest thing to **abandon**, because it requires you to remember to invoke it. Personal dev tools die from friction, not lack of features. The version that *survives* shows up uninvited where review already happens — an **MR comment posted by the pipeline**. Plan: build the TUI first to validate the premise for yourself; the durable endgame is the embedded pipeline step that posts the skeleton automatically.

## 10. Audience

Not unique, and **not gated on category theory or even FP** — that's over-credentialing the desire. What it requires is one instinct: *the meaning is concentrated in the interface, not the implementation.* Common among Rust, ML-family, and serious-TypeScript folks, but also interface-first OO and API-design-minded engineers who'd never call themselves functional. A minority temperament, but a bigger minority than "people who've read about functors."

The distinction that matters: **narrow demand, broad latent benefit.** The people who'd *articulate* wanting this are a minority; the people who'd *use it if handed it* (the engineer drowning in a 900-line AI PR, someone onboarding to an unfamiliar module, a rushed reviewer wanting a cheaper real first pass) are not. That gap is where good devtools live.

The honest catch: the temperament that *wants* this overlaps heavily with the temperament that *needs it least* — type-first people already do this read in their heads, fast. So type-nerd friends loving it is the least informative signal. **The signal that matters is a competent-but-not-type-obsessed engineer reaching for it a second time, unprompted.** To reach that broader pool, the tool eventually has to surface conclusions, not just signatures (see roadmap) — the raw skeleton quietly requires the user to already be you.

## 11. Language generalization (if it gains traction)

Same two dials, expressiveness × honesty:

- **Strong on both — ideal substrate:** Rust; ML family (Haskell is arguably *more* honest than Rust because `IO` is literally in the type); F#, OCaml, Scala, Swift, Elm/ReScript.
- **Steps down:** Java/C# (real signature weight, coarser vocabulary, unchecked exceptions poke holes in honesty); Go (honest-ish via explicit errors, but deliberately coarse types).
- **High expressiveness, low honesty:** TypeScript (discriminated unions are ADTs, but any function can touch ambient state). Strong design-review value, weak reach-honesty.
- **Only via gradual typing:** Python with strict hints + pyright inference recovers value in proportion to annotation discipline. Untyped dynamic languages: thesis doesn't apply.

Extraction itself is portable basically anywhere via LSP; what varies isn't whether you can pull the types, it's how much signal they carry. **Start in Rust; it travels best to other high-expressiveness/high-honesty ecosystems first.**

## 12. The next step before any building

**Run the 30-minute test first.** The whole idea rests on one untested assumption: that reading types-first actually grounds you faster than reading the diff. Two free ways to simulate the tool's output before writing a line of it:

1. Open three recent patches in your editor and **fold all the function bodies** (one keybind in most editors).
2. Run `cargo doc --document-private-items` and browse a module you reviewed last week.

If reading that genuinely grounds you, build it with conviction. If it underwhelms, you've saved yourself a project for the cost of half an hour. The excitement is the *reason* to run the test now — not a reason to skip it.
