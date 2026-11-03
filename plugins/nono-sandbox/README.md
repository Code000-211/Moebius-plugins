# nono-sandbox

A [Moebius](https://github.com/Code000-211/moebius) launcher plugin. It starts every Pi-Bolt process (a card's run, a
chat's Pi) inside a [nono](https://github.com/nolabs-ai/nono) sandbox. The kernel enforces it, without root and without
a daemon: Landlock on Linux, Seatbelt on macOS. Without this plugin in `moebius.toml` nothing changes. The reasons are in
[ADR 0014](https://github.com/Code000-211/moebius/blob/main/docs/adr/0014-a-launcher-plugin-wraps-how-a-harness-starts.md)
of Moebius; the launcher protocol is in its `docs/reference/plugin-api.md#launchers`.

## What a run sees

For every launch the plugin writes a nono profile for that run, owner-only (mode 0600, in a directory with mode 0700), and
answers with `nono --silent run --profile <file> --name <session> --no-audit --no-rollback -- <program> <args>`. The two flags keep nono from leaving an audit record and a session record (each with the whole command line) in the user's state directory for every launch; Moebius keeps its own record of runs.

| | |
|---|---|
| Read and write | The run's worktree, its sessions directory and its temporary directory (the request's `read_write` and `dir`), and the devices `/dev/null`, `/dev/zero`, `/dev/full`, `/dev/tty` with `/dev/fd` and `/proc/self/fd`. |
| Read only | The agent directory and the extensions (the request's `read_only`); the clone whose git directory the worktree uses; the directory of the Pi-Bolt installation (an npm package whole); every existing absolute directory on the run's `PATH` (the toolchains; relative and empty entries are left out); rustup's home and cargo's `registry` and `git` directories when a toolchain of either is on the `PATH` (not the rest of cargo's home, which holds the registry token); the setting `--read-only`; and, on Linux, nono's `system_read_linux_core` group, which every program needs to start. |
| Nothing else | Not Moebius's data directory, `secrets.toml`, the token file or `moebius.toml`, and not `/tmp`. nono adds base groups to every profile whether the profile names them or not; the profile excludes `system_write_linux`, `system_write_macos` (write access to `/tmp`), `user_tools`, `homebrew_linux` and `homebrew_macos`, and on macOS `system_read_macos` (it reads `/opt`, `/var`, `/private`, `/etc`, `/Library`), with a short list of system directories in its place. |
| Unix sockets | `linux.af_unix_mediation` is `pathname` and no socket is granted, so a run cannot connect to the user's D-Bus (`systemd-run --user`), Docker or ssh-agent sockets. Without it nono lets a run connect to any pathname socket. With `--unix-sockets allow` the key is left out, for WSL2 (see below), and the launch note says so. |
| Network, mode `moebius` (default) | All outbound access is blocked except TCP to the ports of the router, the gateway and the tool-call check, which must be on the loopback and 1024 or more (`network.block` with `network.connect_port` on Linux, `network.open_port` on macOS). See the limit below. |
| Network, mode `internet` | Outbound access is not restricted. |
| Environment | `PATH`, `HOME`, `USER`, `SHELL`, `LANG`, `LANGUAGE`, `LC_*`, `TERM`, `TZ`, `TMPDIR`, `TEMP`, `TMP`, `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY` (and lower-case), `SSL_CERT_FILE`, `SSL_CERT_DIR`, `NODE_EXTRA_CA_CERTS`, `CARGO_HOME`, `RUSTUP_HOME`, `PI_CODING_AGENT_DIR`, `MOEBIUS` and `MOEBIUS_*`. Nothing else: no provider keys. The profile lists the same names in `environment.allow_vars`. |

A directory that the plugin grants for reading because of the environment (a `PATH` entry, the directory of the harness,
`--read-only`) and that contains the worktree, the clone, another path the run writes or the profiles directory makes the
launch fail with a sentence that names it: such a grant would make the data directory, its secrets and the other
worktrees readable too (for example `/opt/moebius` on the `PATH` when the data directory is `/opt/moebius/.moebius`).

The environment is the process environment of `nono`, not its arguments, and the profile file holds names, never values:
the run token is in neither argv nor any file.

**The limit of mode `moebius` on Linux.** Landlock filters TCP by port, not by host. The router's, the gateway's and the
check's ports are therefore open on every host: a run can connect to that port number on any internet host (it can not
listen on a network interface, and every other port is closed). The plugin lets only loopback addresses and ports of
1024 or more in (a router at `https://localhost` would open port 443, which is the web), and the launch note
(`launcher_note` event of the run) says this plainly. On macOS Seatbelt limits `open_port` to localhost.

The plugin answers `unchanged`, with a note that Moebius records as a `launcher_note` event of the run, for a run on an
SSH runner and for an excluded agent.

Stop and cleanup: the wrapped process stays in the process group the core made for it, and the core kills that group
after the stop command, which ends nono and the program in it. The stop command (`nono-sandbox stop <nono> <session>`)
also asks `nono stop --force`, which knows only the sessions nono manages itself, and always succeeds. The cleanup
command (`nono-sandbox cleanup <profile>`) removes the profile, and only a file named like one of this plugin's.
`leftovers` names the profiles of runs that are not running, and the core removes them.

## Settings

The core's `[[plugins]]` entry has no table of settings, so they are the entry's `args`:

| Argument | Setting | Default | What it is |
|---|---|---|---|
| `--nono <program>` | `nono` | `nono` | The nono program. |
| `--nono-version <version>` | `nono_version` | `0.81.0` | The nono version the plugin is tested with. Another version stops the plugin at start, since nono is before 1.0 and changes its profiles. |
| `--network moebius\|internet` | `network` | `moebius` | The network mode. |
| `--unix-sockets deny\|allow` | `unix_sockets` | `deny` | Whether nono mediates Unix sockets. `allow` leaves a run free to connect to any socket it can reach by path; only for WSL2, where `deny` cannot work. |
| `--read-only <path>` | `read_only` | none | An extra path the runs may read; repeat for more. Absolute, without `$` or `*`; the plugin does not start otherwise. |
| `--exclude-agent <name>` | `exclude_agents` | none | An agent that runs without the sandbox, such as the Moebius agent, which operates the server with `"$MOEBIUS"`; repeat for more. |
| `--agent-network <name>=<mode>` | `agents.<name>.network` | none | The network mode for one agent; repeat for more. |
| `--profiles-dir <dir>` | | `.moebius-nono-profiles` in the plugin's working directory (that of `moebius.toml`) | Where the profiles are written. Keep it outside every directory a run can read; the launch fails otherwise. |

```toml
[[plugins]]
name = "nono-sandbox"
path = "/opt/moebius/plugins/nono-sandbox"
args = [
  "--exclude-agent", "Moebius",
  "--agent-network", "Researcher=internet",
  "--read-only", "/opt/toolchains",
]
launches = true
```

## Platforms and checks at start

The plugin checks once, when it starts, and exits with a sentence (in `<data_dir>/plugins/<name>.log`) when it cannot
sandbox: a launcher that is not running fails every run, so a run never starts unsandboxed.

- Linux needs Landlock (kernel 5.13 or later with Landlock on). The plugin runs `nono setup --check-only` and requires
  `Landlock enabled` in its output. When the default mode or any `--agent-network` is `moebius` it also requires TCP
  filtering (`TCP network filtering` or `TCP network rule support verified`, Landlock ABI 4, kernel 6.7 or later);
  without it the plugin stops with a sentence instead of failing every run.
- macOS works through Seatbelt, and is **experimental**: the profile for it has not run on a Mac yet, and the end-to-end
  tests run on Linux only.
- Windows is refused: nono supports only WSL2, where the Linux build is used.
- WSL2 is told as nono tells it, from the kernel and never the environment: a kernel release that names Microsoft, or
  `/proc/sys/fs/binfmt_misc/WSLInterop` (or `WSLInterop-late`). It is refused while `--unix-sockets` is `deny`:
  mediating Unix sockets needs a seccomp user-notification listener, and WSL2's kernel answers `EBUSY` for one, so nono
  would exit before every run (Moebius then says "pi-bolt exited before it settled"). Set `--unix-sockets allow` there.
  WSL2's kernel is 6.6, without Landlock ABI 4, so mode `moebius` is refused there too (the TCP check above): set
  `--network internet` and no `--agent-network moebius`. The filesystem limits and the environment stay. The plugin
  uses no nono credential proxy, so nono's `wsl2_proxy_policy` does not apply and is not set.
- `nono --version` must say the version of `--nono-version`.

## Known limits

- The clone is readable but not writable, so git commands in the worktree that write the worktree's index in the clone's
  git directory fail; Moebius commits from outside the sandbox.
- Toolchains: the lens's `cargo check` needs rustup's home and cargo's sources, which are granted read only when a
  toolchain is on the `PATH`; cargo also wants to write its lock file in its home, which is not granted. Not checked
  against a real Pi-Bolt yet.
- Only Pi-Bolt is launched through a launcher today.

## Installing

See the [README of this repository](../../README.md#installing-a-plugin-by-hand). nono is installed separately
(`curl -fsSL https://nono.sh/install.sh | sh`, or a release from `github.com/nolabs-ai/nono/releases`).

## Tests

`cargo test -p moebius-plugin-nono-sandbox` runs the unit tests (the profile, the settings, the checks with a fake nono,
the launch and the leftovers) and `tests/protocol.rs`, which starts the built program and speaks JSON Lines to it. The
end-to-end tests in `tests/e2e.rs` run with a real nono when `MOEBIUS_E2E_NONO` is set (`1`, or the path of nono) on
Linux, in a directory under `target/` (not `/tmp`, which a profile must not grant): a sandboxed script reads and writes
its worktree and nothing else (it cannot write outside it, into the data directory, into the profiles directory or
`/tmp`, nor read another worktree), cannot connect to a Unix socket, reaches the router's port but no other port in
mode `moebius` (also on this machine's non-loopback address), and has no provider key; the resolved profile
(`nono profile show`) has none of the write groups; the stdin and stdout pass through; and after a stop the sandboxed
program itself, not only nono, is gone.
