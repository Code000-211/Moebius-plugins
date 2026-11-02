# moebius-plugins

Plugins and other packages for [Moebius](https://github.com/Code000-211/moebius), the orchestration layer that drives
coding-agent harnesses from a repository's issues to changes ready to merge. Moebius keeps its core generic; what
extends it lives here, and this repository is also meant to become the package index that Moebius installs from
(Moebius issue #172).

## Layout

```
moebius-plugins/
├── plugins/
│   └── nono-sandbox/        a launcher: every Pi-Bolt run in its own nono sandbox
│       ├── moebius-package.toml
│       ├── Cargo.toml
│       └── src/, tests/
├── workflows/<name>/        column-script packages (none yet)
├── rules/<name>/            rules packages (none yet)
├── skills/<name>/           skill packages (none yet)
└── .github/workflows/       CI
```

A plugin is a program in any language that speaks the process plugin protocol of Moebius (see
`docs/reference/plugin-api.md` in the Moebius repository). The ones written in Rust depend on `moebius-plugin-api` by
git revision of the Moebius repository, so that repository stays the source of the API. Each package has a
`moebius-package.toml` that says what it is, which Moebius versions it works with, the settings it takes with their
defaults and the targets it is built for. `index.toml`, the generated list of packages and releases, comes with the
package manager.

## Packages

| Package | Kind | What it does |
|---|---|---|
| [`nono-sandbox`](plugins/nono-sandbox/README.md) | plugin, launcher | Starts every Pi-Bolt process in a [nono](https://github.com/nolabs-ai/nono) sandbox (Landlock on Linux, Seatbelt on macOS): the run sees its worktree and a few read-only paths, and by default reaches only Moebius's ports (on Linux nono filters by port, so those ports are open on every host; macOS is experimental). |

## Installing a plugin by hand

Until the package manager exists, a plugin is built or downloaded and then named in `moebius.toml`.

1. Get the program. Build it from a checkout of this repository, or download the program of a release of the package
   (tag `<plugin>-v<version>`) once releases are published:

   ```sh
   cargo build --release -p moebius-plugin-nono-sandbox
   # the program is target/release/nono-sandbox
   ```

2. Add a `[[plugins]]` entry. A plugin that wraps how harnesses start has `launches = true` (at most one plugin has
   it); a plugin's settings are the entry's `args`:

   ```toml
   [[plugins]]
   name = "nono-sandbox"
   path = "/opt/moebius/plugins/nono-sandbox"
   args = ["--network", "moebius", "--exclude-agent", "Moebius"]
   launches = true
   ```

3. Restart the server. The plugin is started with it; the Plugins page of the app and `GET /v1/plugins` show whether it
   runs. A plugin that cannot start (its tool is missing, its platform is not supported) says why in
   `<data_dir>/plugins/<name>.log`, and a launcher that is not running fails every run it is asked about rather than
   letting it start unwrapped.

## Working on it

`cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` must pass; CI runs
the same on Linux and macOS, and an end-to-end job with a real nono. See [CLAUDE.md](CLAUDE.md).

## License

Apache-2.0, as Moebius. See [LICENSE](LICENSE).
