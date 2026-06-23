# absolem — Rust Style & Conventions

> The **architectural** rules below are load-bearing — binding regardless of
> what Rust can enforce. The lint baseline is set in `Cargo.toml` `[lints]`
> and is the source of truth; this doc explains the *why* behind the
> load-bearing choices. Rustfmt knobs and naming micro-rules remain
> deliberately under-specified — the human PM will sharpen them over time.
> Don't lock in taste-level choices ahead of explicit guidance.

Read alongside [`DESIGN.md`](./DESIGN.md) (architecture).

---

## 1. Toolchain

- **Stable Rust, 2024 edition.** Pin via `rust-toolchain.toml` once the
  walking-skeleton MR lands.
- `cargo fmt` and `cargo clippy` are not optional. CI runs `cargo fmt
  --check` and `cargo clippy -- -D warnings`.
- No nightly in the build. If a future Tier 2 Rust producer needs nightly
  rustdoc, it is invoked as an external process behind a capability (see
  §3), never as a build toolchain.

## 2. Formatting

rustfmt is law. Do not hand-format, do not fight it, do not add
`#[rustfmt::skip]` without a one-line comment justifying it. PR diffs
should never contain formatting noise. Specific `rustfmt.toml` knobs are
TBD — default rustfmt until the human says otherwise.

## 3. Capabilities & side effects — the load-bearing architectural rule

**Side effects are modeled as capability injection. No function reaches for
ambient authority.** To perform an effect, a function must be *handed* the
capability to do it. A function whose signature contains no capability
provably cannot perform that effect. This is the object-capability model:
effects visible in signatures, purity by construction.

Rust does not enforce this for free, so **the pattern is the contract**,
backstopped by lints and dependency hygiene. It is binding regardless.

### 3.1 The rules

1. **Pure by default.** Core logic (the IR types, the diff engine) takes
   no ambient effects. A function that needs an effect takes a capability
   parameter.
2. **Inject via generic bounds (`impl Trait` / `<C: Cap>`), not globals.**
   Static dispatch is the default; `dyn` only where erasure is genuinely
   needed (e.g. a producer registry).
3. **Pure code must not depend on effectful APIs at all.** The IR types
   and diff engine do not import `std::fs`, `std::net`, `std::process`,
   `std::time`, or any IO/network third-party crate. If it's not
   reachable, it can't be called. Structural exclusion via the
   dependency graph (when there's more than one crate) is the strongest
   enforcement; within a single crate, module discipline plus lints
   stand in.
4. **One composition root.** Only the binary entry point constructs real
   capabilities and threads them inward. Effects exist at exactly one
   edge.

### 3.2 The capability seams

The effectful seams `absolem` will need are capability traits, owned by
the code that consumes them:

- `Clock` — current time.
- `FileSystem` — read / list.
- `ProcessRunner` — run subprocesses (git, gopls, language servers).
- `GitRepo` — resolve refs, read trees (built on `ProcessRunner` or gitoxide).
- `LspClient` — navigation and Tier 1 enrichment.

These are sketched, not committed. Each lands as code arrives that needs it.

### 3.3 Shape

```rust
// Core — a capability, not ambient authority.
pub trait ProcessRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<Output, RunError>;
}

// A function receives the capability; it cannot spawn a process any other way.
pub fn read_tree_at(
    runner: &impl ProcessRunner,
    repo: &Path,
    rev: &str,
) -> Result<Tree, ReadError> { /* … */ }
```

```rust
// Composition root — the ONLY place real effects are constructed.
let runner = RealProcessRunner;     // thin wrapper over std::process::Command
let clock  = SystemClock;           // thin wrapper over SystemTime::now
let surface = extract_go(&runner, &repo, &rev)?;
```

### 3.4 Enforcement backstop

Two layers, both backstopping the same rule:

**Lints (`Cargo.toml` `[lints]`)** catch ambient effects every layer should
avoid by default — `print_stdout`, `print_stderr`, `exit`, `unsafe_code` —
and the panic family (`unwrap_used`, `expect_used`, `panic`, `todo`,
`unimplemented`). CI runs `-D warnings`, so warns become hard failures.

**`clippy.toml`** closes the gap for calls the lint baseline can't catch by
name alone — the `disallowed-types` / `disallowed-methods` mechanism for
ambient std APIs. Starting pattern:

```toml
# clippy.toml — at project root today; per-crate once there's a workspace.
disallowed-types = [
  { path = "std::process::Command",      reason = "inject a ProcessRunner" },
]
disallowed-methods = [
  { path = "std::time::SystemTime::now", reason = "inject a Clock" },
  { path = "std::time::Instant::now",    reason = "inject a Clock" },
  { path = "std::fs::read",              reason = "inject a FileSystem" },
  { path = "std::fs::read_to_string",    reason = "inject a FileSystem" },
  { path = "std::fs::write",             reason = "inject a FileSystem" },
]
```

Together with structural exclusion (§3.1.3), this gives three layers of
enforcement: lints, disallowed lists, and reachability through the
dependency graph. The strongest is the last.

### 3.5 Payoff (so it isn't read as ceremony)

Because effects are injected, core logic is testable with trivial
in-memory fakes — no temp dirs, real clocks, subprocesses, or network in
unit tests. A test that needs IO is a smell: a capability wasn't injected.

## 4. Error handling

- **Library code:** typed errors via `thiserror`. No stringly-typed errors
  at module / crate boundaries.
- **Binary entry point:** `anyhow` for top-level wiring and human-facing
  context.
- **No `unwrap` / `expect` / `panic` in library paths.** The rare invariant
  that truly cannot fail uses `expect("explicit reason")`, with the reason
  documenting *why* it's unreachable.

## 5. Naming & modules

- Use project vocabulary exactly: `Surface`, `Item`, `ItemId`, `TypeRef`,
  `ChangeSet`, `Producer`, `Tier`, `Frontend`. No synonyms.
- One concept per file. Prefer `foo.rs` + `foo/` submodules over `mod.rs`.
- Default to `pub(crate)`; promote to `pub` only when intended surface.
  (We are a tool about minimal API surfaces.) The `unreachable_pub` lint
  backs this.

## 6. Generics vs. trait objects

Prefer generics / `impl Trait` for capabilities (the §3 pattern). Reach for
`dyn` only where heterogeneity is required — chiefly a producer registry,
where Surfaces from different languages flow through one path. Keep
capability traits object-safe so `dyn` remains available.

## 7. Documentation

- The IR types, when extracted into a library boundary, should deny
  `missing_docs` — the contract is documented exhaustively. Override
  `missing_errors_doc` / `missing_panics_doc` back to `warn` there
  (they're `allow`-ed at the baseline for the rest of the code).
- Every capability trait documents *what authority it grants and why it's
  a seam*, not just its methods.

## 8. Dependencies

Adding a dependency to pure code is a design event — justify it in the MR,
especially anything that could smuggle in ambient effects.
