# Documentation layering pitfalls

## Putting advanced/strict APIs in the beginner tutorial

`docs/guide/tutorial/` is a curated, smooth learning curve. Dropping a strict or
rarely-needed API into an early chapter raises the cognitive load for every new
reader. Put it in the relevant `docs/guide/` reference page and, if needed, leave a
single forward pointer from the tutorial. Protecting the first-run experience is a
deliberate project rule.

## Confusing user-facing guide content with internal architecture

`docs/guide/` answers "how do I use X"; `docs/architecture/` answers "how does X
work inside". A user looking up `state-management` should not have to wade through
the StateManager epoch/notify internals — those belong in
`docs/architecture/lifecycle-management.md`. Keep usage and mechanism apart.

## Adding a tutorial chapter without linking it from `quick-start.md`

The tutorial is a *sequence*. A new lesson file under `docs/guide/tutorial/` that
isn't added to the `quick-start.md` chapter list is orphaned — readers following
the path never reach it. Always wire a new chapter into the index.

## Treating `docs/development/` and `docs/architecture/` as interchangeable

`docs/development/extending-framework.md` is *actionable* maintainer guidance
("to add a trigger host, do this"). `docs/architecture/` is *explanatory* ("here is
why the supervisor FSM has these states"). A how-to-extend page is development; a
how-it-works page is architecture.

## Documenting a capability only in the tutorial

If a capability has a topical reference home (`docs/guide/<topic>.md`), the
authoritative description belongs there. The tutorial should *teach* it in context
and link out, not be the only place it's documented — otherwise reference readers
can't find it without replaying the whole lesson sequence.
