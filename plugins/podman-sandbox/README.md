# podman-sandbox

A [Moebius](https://github.com/Code000-211/moebius) launcher plugin. It starts every Pi-Bolt process (a card's run, a
chat's Pi) in a rootless [Podman](https://podman.io) container of an image you build for the project: the project's
dependencies, toolchains and tools (a browser for Playwright, say) are in the image, and the run sees nothing of the host
but its own paths. Without this plugin in `moebius.toml` nothing changes. The reasons for launchers are in
[ADR 0014](https://github.com/Code000-211/moebius/blob/main/docs/adr/0014-a-launcher-plugin-wraps-how-a-harness-starts.md)
of Moebius; the launcher protocol is in its `docs/reference/plugin-api.md#launchers`.

Where [`nono-sandbox`](../nono-sandbox/README.md) limits what a run may do on the host, with the host's tools, this
plugin gives a run its own system: what it needs is installed in the image, not granted from the host.

## What a run sees

For every launch the plugin answers with itself, `podman-sandbox run`, around
`podman run --rm --interactive --init --pull never --log-driver none --userns keep-id --cap-drop ALL --security-opt no-new-privileges … <image> /bin/sh -c <script> moebius-env <pi-bolt> <args>`.

| | |
|---|---|
| Read and write | The run's worktree, its agent directory, its sessions directory and its temporary directory (the request's `read_write` and `dir`), at the same paths as on the host. The run is the server's user (`keep-id`), so what it writes is the user's. |
| Read only | The lens and the bridge and the agent's own extensions and skills (the request's `read_only`), the clone whose git directory the worktree uses, and Pi-Bolt's installation, each at its host path. |
| Its own | The image (its `PATH`, its toolchains, its certificates), its own `/tmp`, a fresh `HOME` at `/home/moebius` (a tmpfs of the run) and the `--cache` volumes. Everything the run changes outside the mounts goes with the container. |
| Nothing else | Not Moebius's data directory, `secrets.toml`, the token file or `moebius.toml`, not other worktrees, not the user's home, not the host's `/tmp` and no socket of the host. |
| Network, mode `moebius` (default) | pasta with `--splice-only`: the container has no interface but its loopback, on which the ports of the router, the gateway and the tool-call check are forwarded to the same ports of the host's loopback. Nothing else answers: no other port of the host, no other machine, no DNS. Unlike nono on Linux this holds by host, not by port. |
| Network, mode `internet` | The same forwarded ports, and general outbound access through pasta. The host's other loopback ports stay out of reach. |
| Environment | `HOME` (`/home/moebius`), `LANG`, `LANGUAGE`, `LC_*`, `TERM`, `TZ`, `TMPDIR`, `TEMP`, `TMP`, `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY` (and lower-case), `PI_CODING_AGENT_DIR`, `MOEBIUS` and `MOEBIUS_*`, and the image's own. No provider keys. |

**The run token** reaches the container through its standard input, not through Podman. `podman-sandbox run` starts
Podman with its own environment minus the run's variables. It writes those variables to the container's standard input
as `NAME=value` lines and an empty line, then relays Moebius's standard input. In the container, `/bin/sh` exports each
line (`export "$line"`, which assigns and never evaluates) and becomes Pi-Bolt, which reads the rest. The token is
therefore in none of Podman's arguments, its environment, its container configuration (`podman inspect`) or a log. A
value with a line break cannot be passed this way, so it fails the launch with the variable's name.

**Pi-Bolt** is run from the host, by its real path, so its version stays that of the server:

- An installation directory of its own (`~/.pi-bolt/pi-bolt-linux-x64/`, say) is mounted whole.
- A program in a `bin` directory is mounted alone.
- A program in a `node_modules` package gets its package mounted, and the image then needs `node`.
- A Pi-Bolt under `/usr`, `/bin`, `/sbin`, `/lib`, `/lib64` or `/etc` would be mounted over the image's own directories,
  so that fails the launch. So does an installation directory that contains the worktree, the clone or another path the
  run writes.

The prebuilt Pi-Bolt is a glibc program, so the image must be glibc-based, not Alpine.

Every host path is mounted with `z`, which labels it for containers where SELinux is on (Fedora) and does nothing
elsewhere. The plugin answers `unchanged`, with a note, for a run on an SSH runner and for an excluded agent. Every
wrapped launch has a note naming its container, its image and what its network reaches; Moebius records it as the
run's `launcher_note` event.

Stop and cleanup:

- The stop command (`podman-sandbox stop <podman> <container>`) is `podman rm --force --time 0 --ignore`. It always
  succeeds, and only for a container named like one of this plugin's.
- The container has `--rm`, so a container that ends on its own leaves nothing.
- Containers are labelled `moebius.plugin=podman-sandbox` and `moebius.run=<id>`. At the server's start, `leftovers`
  names the containers of runs that are not running, and the core removes them.

## Settings

The core's `[[plugins]]` entry has no table of settings, so they are the entry's `args`:

| Argument | Setting | Default | What it is |
|---|---|---|---|
| `--podman <program>` | `podman` | `podman` | The Podman program. |
| `--image <image>` | `image` | none | The image of a run whose repository has none of its own, and of a chat without a repository. |
| `--repo-image <repository>=<image>` | `repositories.<name>.image` | none | The image of one repository's runs; repeat for more. The repository is told by the name of its clone's directory, `<data_dir>/repos/<name>`, which is its name in Moebius. |
| `--network moebius\|internet` | `network` | `moebius` | The network mode. |
| `--agent-network <name>=<mode>` | `agents.<name>.network` | none | The network mode for one agent; repeat for more. |
| `--cache <volume>:<path>` | `caches` | none | A named Podman volume mounted at an absolute path of the container and kept between runs, such as a package manager's cache; repeat for more. Every run of every agent shares it, so one run can leave something in it that the next one uses. Prefer caches that check what they hold (npm, cargo and pip check checksums). |
| `--exclude-agent <name>` | `exclude_agents` | none | An agent that runs without the sandbox, such as the Moebius agent, which operates the server with `"$MOEBIUS"`; repeat for more. |

At least one image must be set. A run whose repository has no image while there is no `--image` fails with a sentence
saying which.

```toml
[[plugins]]
name = "podman-sandbox"
path = "/opt/moebius/plugins/podman-sandbox"
args = [
  "--image", "localhost/moebius-base:latest",
  "--repo-image", "shop=localhost/shop:latest",
  "--agent-network", "Researcher=internet",
  "--cache", "moebius-npm:/home/moebius/.npm",
  "--exclude-agent", "Moebius",
]
launches = true
```

## The image

Any glibc-based image that has `/bin/sh`, with what the project's runs need: its language and package manager, its
dependencies, the tools its tests use. Install tools outside `HOME`, since `/home/moebius` is empty at every run (for
Playwright set `PLAYWRIGHT_BROWSERS_PATH=/ms-playwright` before `npx playwright install --with-deps`). The run's user is
the server's uid, not the image's `USER`, so make what a run writes outside its mounts writable for it, or leave it to
`/tmp`. For example:

```dockerfile
FROM docker.io/library/node:24-bookworm
ENV PLAYWRIGHT_BROWSERS_PATH=/ms-playwright
RUN npx -y playwright install --with-deps chromium && chmod -R a+rX /ms-playwright
```

```sh
podman build -t localhost/shop:latest -f Containerfile .
```

The plugin never pulls (`--pull never`): an image is in Podman's storage before the server starts, or the plugin does not
start. Building a new image under the same name takes effect at the next run.

## Platforms and checks at start

The plugin checks once, when it starts, and exits with a sentence (in `<data_dir>/plugins/<name>.log`) when it cannot
sandbox: a launcher that is not running fails every run, so a run never starts outside a container.

- Linux only, where rootless Podman needs no virtual machine. Other platforms are refused.
- `podman info` must answer, and say that Podman is rootless (a container of root's Podman would not be the user's).
- Podman 4.4 or later, the first that passes pasta's options through, and pasta (the package `passt`), which Podman 5
  names in `podman info` and Podman 4 finds on the `PATH`.
- Every image of `--image` and `--repo-image` exists (`podman image exists`).

The wrapped Podman, the stop command and `leftovers` get the plugin's own `PATH`, `HOME`, `XDG_RUNTIME_DIR`,
`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `DBUS_SESSION_BUS_ADDRESS`, `CONTAINERS_CONF`, `CONTAINERS_STORAGE_CONF` and
`CONTAINER_HOST`, so that they find the storage the checks saw.

## Known limits

- The clone is read only, so git commands in the worktree that write the clone's git directory (the worktree's index)
  fail; Moebius commits from outside the sandbox.
- The repository's image is chosen by its clone's directory name, which the launcher protocol does not promise; a
  `repository` field in the launch request would be the stable way.
- Every container shares the host's kernel. A stronger boundary is a runtime with its own kernel (gVisor's `runsc`, or
  `krun` for a micro-VM per run), which this plugin does not set yet.
- Only Pi-Bolt is launched through a launcher today.

## Installing

See the [README of this repository](../../README.md#installing-a-plugin-by-hand). Podman and pasta are installed
separately (`dnf install podman passt`, `apt install podman passt`).

## Tests

`cargo test -p moebius-plugin-podman-sandbox` runs the unit tests and `tests/protocol.rs`. The unit tests cover the
container's arguments, the settings, the checks against a fake Podman, the launch and the leftovers. The protocol test
starts the built program and speaks JSON Lines to it, then runs the wrapped launch against a fake Podman and checks that
the token reaches the harness but not Podman.

The end-to-end tests in `tests/e2e.rs` run with a real rootless Podman when `MOEBIUS_E2E_PODMAN` is set (`1`, or the path
of Podman), on Linux. They use the image `MOEBIUS_E2E_PODMAN_IMAGE` (default `docker.io/library/debian:stable-slim`,
pulled beforehand), in a directory under `target/`. They check that:

- a run reads and writes its worktree and temporary directory as the user, and reads its read-only paths without writing
  them;
- it sees neither the data directory's secrets, nor another worktree, nor this repository;
- it has its token and no provider key;
- it reaches the router's port and no other port of the host or of this machine's network address, in both modes;
- the standard input and output pass through;
- the stop command removes the container and ends the launch;
- a container whose launch was killed with its process group is named by `leftovers`.
