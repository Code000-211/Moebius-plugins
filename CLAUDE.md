# Working on moebius-plugins

This repository holds the plugins and packages of [Moebius](https://github.com/Code000-211/moebius). The conventions
are those of that repository (its `CLAUDE.md` and `CONTRIBUTING.md`); this file only says what differs here.

## Layout

- `plugins/<name>/` is one plugin, its own crate in the Cargo workspace, with a `moebius-package.toml` and a `README.md`
  that says what it does, its settings and how it is installed.
- `workflows/`, `rules/` and `skills/` are for packages that are not programs; none yet.
- A plugin in Rust depends on `moebius-plugin-api` with `git` and a full `rev` of the Moebius repository, never a branch.
  Move the `rev` deliberately, in a commit of its own.

## Style

- Every module starts with a `//!` doc comment that says what it is for and how it fits in. Doc comments say what a thing
  is for in plain sentences; little else in comments. Names read as words.
- Build only what the change needs: no options nobody sets, no trait with one implementation.
- Every behaviour has a test; a bug fix has a test that fails without the fix. Tests are not flaky: no tight time limits,
  no fixed ports (bind to port 0), no sleeps where a condition can be awaited. Fake outside programs with a script in a
  temporary directory (a Unix-only test says why with `#[cfg(unix)]`).
- Errors are `thiserror` enums per module. Anything that fails while deciding fails closed: a launcher that cannot
  sandbox a run fails it, it never lets it start bare.
- Secrets never reach a log or a file. The run token is in the environment of a wrapped process, never in its arguments.
- Never put a model name in a commit, code or comment.

## Checks

Run all three before every commit; CI runs the same:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The end-to-end tests of `nono-sandbox` run only when `MOEBIUS_E2E_NONO` is set (`1` for the `nono` on `PATH`, or the
path of the program), on Linux. CI installs the pinned nono and sets it.

## Commits

One commit per change, imperative subject, a body that says what and why, then `Refs Code000-211/Moebius#<n>` or
`Closes ...` and the session's attribution trailers.
