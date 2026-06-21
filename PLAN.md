# absolem — working plan

> Open this file every time you sit down. Update "Where I am" and "Next action" before you close your laptop.
>
> The thesis lives in `DESIGN.md`. Don't re-read it as procrastination — only when you've forgotten *why*.

---

## Where I am

§12 test passed — type-first reading grounds me. Project is greenlit.
Current rung: **(not yet started)** Step 1 — diff-parser CLI.

## Next action

Pick one concrete thing. Replace this line every session. Examples of "concrete":

- "Get `git diff --unified=0 --no-color` running from `std::process::Command` and printing raw output."
- "Parse one `@@ -a,b +c,d @@` header into a struct."
- "Print `(file, [line ranges])` for a real patch from this repo's history."

**Right now:** Decide on a binary name (placeholder is `absolem` from `cargo new`; `skel` / `skim` / `typediff` floated in the doc). Then start step 1.

---

## The ladder

Tick boxes as you ship. Each rung ends with **something that runs end-to-end on a real input.** No half-shipped rungs.

### [ ] 1. Diff-parser CLI
- [ ] Shell out to `git diff --unified=0 --no-color` via `std::process::Command`
- [ ] Parse `@@ -a,b +c,d @@` hunk headers
- [ ] Print `(file path, [(start_line, line_count), ...])` to stdout
- [ ] Run it on a real diff from another repo and eyeball the output
- **Done when:** you've used it on something real once.

### [ ] 2. Signature extractor CLI (`syn`)
- [ ] Add `syn` with the `full` feature
- [ ] For each changed file, parse with `syn::parse_file`
- [ ] Walk items (`ItemFn`, `ItemStruct`, `ItemEnum`, `ItemImpl`, …); for each, check whether its span overlaps any changed range from step 1
- [ ] Print full signatures of touched items (this is where multi-line `fn` + `where` clauses come along for free)
- [ ] First snapshot tests with `insta` against fixture diffs
- **Done when:** running it on a real PR gives you a usable orientation pass for small patches.

### [ ] 3. Closure as indented text (crude resolver)
- [ ] Define `trait Resolver { fn resolve(&self, name: &str) -> Option<Def>; }` *before* writing either impl
- [ ] Crude impl: grep workspace for `struct`/`enum`/`type`/`trait <name>` declarations
- [ ] Walk types mentioned in each changed signature; pull definitions for workspace types; treat std/third-party as leaves
- [ ] Cycle detection (mark "already shown")
- [ ] Print nested-indented, with context types visually marked (e.g. dimmed prefix or `[ctx]` tag)
- [ ] Snapshot tests on fixtures that exercise re-exports / shared types
- **Done when:** the output actually grounds you on a real PR you reviewed recently.

### [ ] 4. TUI (`ratatui`)
- [ ] Feed the same data structure from step 3 into a tree widget
- [ ] Collapsible nodes, lazy expansion, j/k navigation
- [ ] Don't redesign the data — presentation upgrade only
- **Done when:** you reach for the TUI over the CLI for a real review, unprompted.

### [ ] 5. RA-backed resolver swap
- [ ] Second `Resolver` impl driving `rust-analyzer` over LSP (`textDocument/definition`, `hover`)
- [ ] Manage RA process lifecycle; keep it warm
- [ ] Swap behind a CLI flag; crude resolver remains as fallback
- **Done when:** RA-resolver handles re-exports / aliases that crude resolver gets wrong.

---

## Yak watch (anti-scope-creep)

Things to **not do**, with the rung where they become legal. If you catch yourself doing one before its rung, you're yak-shaving.

- **Don't reach for `git2` or `gix`.** Shell out via `Command` for step 1. Revisit only if/when the two-revision (before/after) work in §8 lands — that's the only point where libgit2's structured object access pays off.
- **Don't design the `Resolver` trait until step 3.** Resist building "the architecture" for it during step 2. The shape isn't real until you have a concrete use site.
- **Don't touch `ratatui` until step 3 ships to your satisfaction.** TUI before correct data = you debug presentation while the underlying logic is still wrong.
- **Don't read `ra_ap_*` source.** Ever, until LSP demonstrably can't give you what you want. The doc says this twice for a reason.
- **Don't go after the two-revision (before/after) problem before step 3 is real.** §5 explicitly defers it.
- **Don't generalize to other languages, frameworks, configurability, or plugin systems.** §11 is a roadmap musing, not a backlog item.
- **Don't add a config file until you've copy-pasted a CLI flag 3+ times.**
- **Don't refactor a working rung to make the next one "cleaner".** Build the next one on top of what works; refactor only if it actively blocks you.

If you're about to do one of these and feel you have a good reason — write the reason in the Decisions log below before doing it. Forcing the justification is half the firewall.

---

## Decisions log

Append-only. Date each entry. Record forks taken and *why*, especially when you depart from the plan above.

- `YYYY-MM-DD` — Example: "Chose to shell out to `git` rather than use `git2` for step 1. Reason: smaller dep surface, plumbing commands are stable, defer libgit2 until two-revision work needs it."

---

## Session log

Append-only. Date each session. Two lines is fine — one for what you did, one for what's loaded in your head for next time.

- `YYYY-MM-DD` — Example: "Got step 1 printing hunk ranges for the current repo. Next: try on a noisier repo with renames."

---

## Reference

- Design thesis & rationale: `DESIGN.md`
- Run the §12 grounding test before doubting the project: `DESIGN.md` §12
