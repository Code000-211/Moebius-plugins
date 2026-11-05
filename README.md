# moebius-plugins

Plugins and other packages for [Moebius](https://github.com/Code000-211/moebius), the orchestration layer that drives
coding-agent harnesses from a repository's issues to changes ready to merge. Moebius keeps its core generic; what
extends it lives here, and this repository is also meant to become the package index that Moebius installs from
(Moebius issue #172).

## Layout

```
moebius-plugins/
├── plugins/
│   ├── nono-sandbox/        a launcher: every Pi-Bolt run in its own nono sandbox
│   │   ├── moebius-package.toml
│   │   ├── Cargo.toml
│   │   └── src/, tests/
│   └── podman-sandbox/      a launcher: every Pi-Bolt run in a rootless Podman container of the project's image
├── workflows/<name>/        column-script packages (none yet)
├── rules/<name>/            rules packages (none yet)
├── skills/<name>/           skill packages (none yet)
├── tools/index/             the generator of index.toml
├── rust-toolchain.toml      the pinned Rust that CI and releases build with
├── index.toml               the package index, written by the release workflow (lists no package until the first release)
└── .github/workflows/       CI (ci.yml) and releases (release.yml)
```

A plugin is a program in any language that speaks the process plugin protocol of Moebius (see
`docs/reference/plugin-api.md` in the Moebius repository). The ones written in Rust depend on `moebius-plugin-api` by
git revision of the Moebius repository, so that repository stays the source of the API. Each package has a
`moebius-package.toml` that says what it is, which Moebius versions it works with, the settings it takes with their
defaults and the targets it is built for. The format is in `docs/reference/packages.md` of the Moebius repository.

## Packages

| Package | Kind | What it does |
|---|---|---|
| [`nono-sandbox`](plugins/nono-sandbox/README.md) | plugin, launcher | Starts every Pi-Bolt process in a [nono](https://github.com/nolabs-ai/nono) sandbox (Landlock on Linux, Seatbelt on macOS): the run sees its worktree and a few read-only paths, and by default reaches only Moebius's ports (on Linux nono filters by port, so those ports are open on every host; macOS is experimental). |
| [`podman-sandbox`](plugins/podman-sandbox/README.md) | plugin, launcher | Starts every Pi-Bolt process in a rootless [Podman](https://podman.io) container of an image built for the project, with its dependencies and tools (Linux): the run sees its worktree and a few read-only paths, and by default reaches the internet too (the mode `moebius` limits it to Moebius's ports on the host's loopback). |

## The index

`index.toml` at the root of this repository is the package index that Moebius reads (the registry `moebius-plugins`
is always configured). It lists every released version of every package: its manifest fields, and for a plugin the
download of its program for each target with the sha256 that Moebius checks before it writes a file. Nobody edits it:
the release workflow adds a version and the versions of earlier releases stay. Moebius refuses an index whose entry is
not valid as a whole, and the CI job `index.toml` checks the same rules.

**Until the first release `index.toml` lists no package** (it holds only `format = 1`), so nothing can be downloaded:
the format has no entry without a download (Moebius refuses a plugin that has none for its targets), so `nono-sandbox`
is not listed before it has a release. Until then `moebius pkg search` shows nothing from this registry and
`moebius pkg install nono-sandbox` answers that no registry has the package; the first release adds it.

## Installing with Moebius

```sh
moebius pkg search sandbox
moebius pkg install nono-sandbox      # or nono-sandbox@0.1.0
moebius pkg update nono-sandbox
```

Moebius downloads the program for its own target, checks its sha256 against the index, writes it to
`<data_dir>/packages/nono-sandbox/<version>/` and adds the `[[plugins]]` entry. A launcher is read at the server's
start, so the answer says to restart. Workflow, rules and skills packages are proposed as a change that a person merges.

## Publishing a package

1. Add or change the package under `plugins/<name>/` (or `workflows/`, `rules/`, `skills/`; the README in each says
   what a package there looks like). Set `version` in its `moebius-package.toml`; a released version never changes, so
   a change after a release is a new version.
2. Merge it to `main` (CI must pass).
3. Tag the commit `<package>-v<version>` and push the tag, or run the **Release** workflow by hand with the package and
   version:

   ```sh
   git tag nono-sandbox-v0.1.0 && git push origin nono-sandbox-v0.1.0
   ```

The Release workflow, in this order:

1. **Checks the commit** (`github.sha`, the one built and indexed): it must be on `main`, and the tag, if it exists,
   must point at it (a released version does not move). A run started by hand creates the tag at that commit, for every
   kind of package, because the files of a workflow, rules or skills package are named by URLs at the tag. It checks
   that the tag matches the manifest's version.
2. **Builds** a plugin for each of the manifest's `targets` on a runner of that system (`x86_64`/`aarch64` Linux,
   macOS; Windows only for a plugin that lists it) with the toolchain pinned in `rust-toolchain.toml`.
3. **Works out the index entry first**, on a copy of `index.toml`, from the programs that were built, and checks it. A
   version that is in the index already with other contents (a rebuild that differs) stops the run here, before the
   release is touched.
4. **Publishes** the programs, named `<program>-<target>`, with a `SHA256SUMS`, to the GitHub release of the tag. It never
   overwrites: a program that is on the release already must have the sha256 of the build, or the run fails. A re-run
   therefore uploads only what is missing.
5. **Verifies** by downloading the release back: `sha256sum -c SHA256SUMS`, and the sha256 of each program against the
   entry.
6. **Commits** `index.toml` to `main` with the repository token. `main` must accept pushes from it; such a push does not
   start CI, which is why the entry was checked (`check`) in step 3.

A package that is not a program has no build and no upload: its entry names its files at the tag. The jobs have read
access only, except the first (it writes the tag of a run started by hand) and the last.

The generator is `tools/index` (`cargo run -p moebius-plugins-index -- plan|add|check`); its tests run with the rest.

## Installing a plugin by hand

A plugin can also be built or downloaded and then named in `moebius.toml` without the package manager.

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
the same on Linux and macOS, and end-to-end jobs with a real nono and a real rootless Podman. See [CLAUDE.md](CLAUDE.md).

## License

Apache-2.0, as Moebius. See [LICENSE](LICENSE).
