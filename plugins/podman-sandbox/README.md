# podman-sandbox

A [Moebius](https://github.com/Code000-211/moebius) launcher plugin. It starts every Pi-Bolt process (a card's run, a
chat's Pi) in a rootless [Podman](https://podman.io) container of an image made for the project, from a Dockerfile you
give the repository on its page in Moebius's web app or from an image you build yourself: the project's
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

## The Dockerfiles

The plugin asks for two Dockerfiles, both in the web app in a box named `podman-sandbox`, and a person writes them there
and saves them; the next run uses them. The box is the plugin's own page: above each Dockerfile, "Start from" offers
presets that replace its text, to edit before saving.

| Preset | What it is | Offered for |
|---|---|---|
| The server's base | `ARG MOEBIUS_BASE` and `FROM ${MOEBIUS_BASE}`, to add what the project needs | a repository |
| Debian | `node:24-bookworm-slim` with `git` | both |
| Ubuntu | Ubuntu 24.04 with `git`, and Node 24 copied from its official image | both |
| Fedora | Fedora 44 with `git`, and Node 24 copied from its official image | both |
| Playwright | `node:24-bookworm` with Chromium and its system libraries in `/ms-playwright` | both |

The presets are the files of [`page/presets/`](page/presets/); the page is [`page/settings.js`](page/settings.js).

- **The base Dockerfile** is the app setting `base_dockerfile`, on the Settings page. It is what the whole server shares:
  what the harness needs in a container (a glibc-based system with `/bin/sh`, `node` if Pi-Bolt is a node package, `git`)
  and the tools every project uses.
- **The repository's Dockerfile** is the project setting `dockerfile`, on the repository's page (Projects, then the
  repository). It carries only what that project needs, and starts from the base:

  ```dockerfile
  ARG MOEBIUS_BASE
  FROM ${MOEBIUS_BASE}
  RUN apt-get update && apt-get install -y --no-install-recommends python3 && rm -rf /var/lib/apt/lists/*
  ```

  The plugin gives the build the base image's name as the build argument `MOEBIUS_BASE`. Without a base Dockerfile
  the argument is not given, so a repository's Dockerfile then names its own `FROM`.

The image of a run is the first that applies: the repository's Dockerfile, its `--repo-image`, the base Dockerfile
itself, then `--image`. Left empty, the next one applies.

- The image is named by the Dockerfile's hash, `localhost/moebius-podman-sandbox:<first 32 hex digits of its SHA-256>`,
  so the same text is built once and a changed one is a new image. A repository's image built on a base is named by
  both, so a changed base builds every repository's image again at its next run.
- Each is built by the wrapped launch, `podman-sandbox run --build <image> <directory> [--base <image>]`, the base first,
  before the container starts and only when Podman does not have it yet: the first run after a change waits for the build, and the build's output is in
  the run's log. A build that fails fails the run, with a sentence saying so.
- The build context is a directory that holds the Dockerfile alone (one for each, removed when the run ends), so `COPY` and `ADD` of
  local files find nothing; the worktree is mounted at run time, not baked in. `FROM` pulls its base image as Podman's
  `build` does (`--pull missing`), with the network of the server's user.
- Built images are labelled `moebius.plugin=podman-sandbox`; the plugin does not remove the ones a changed Dockerfile left
  behind (`podman image prune --all --filter label=moebius.plugin=podman-sandbox` does).
- Only a person can change them, never an agent: Moebius refuses the Moebius agent's change of a project or app setting.

## Settings

The core's `[[plugins]]` entry has no table of settings, so they are the entry's `args`:

| Argument | Setting | Default | What it is |
|---|---|---|---|
| `--podman <program>` | `podman` | `podman` | The Podman program. |
| `--image <image>` | `image` | none | The image of a run whose repository has none of its own, and of a chat without a repository. |
| `--repo-image <repository>=<image>` | `repositories.<name>.image` | none | The image of one repository's runs, by its name in Moebius, when it has no Dockerfile of its own; repeat for more. |
| `--network moebius\|internet` | `network` | `moebius` | The network mode. |
| `--agent-network <name>=<mode>` | `agents.<name>.network` | none | The network mode for one agent; repeat for more. |
| `--cache <volume>:<path>` | `caches` | none | A named Podman volume mounted at an absolute path of the container and kept between runs, such as a package manager's cache; repeat for more. Every run of every agent shares it, so one run can leave something in it that the next one uses. Prefer caches that check what they hold (npm, cargo and pip check checksums). |
| `--exclude-agent <name>` | `exclude_agents` | none | An agent that runs without the sandbox, such as the Moebius agent, which operates the server with `"$MOEBIUS"`; repeat for more. |

A run whose repository has no Dockerfile and no `--repo-image`, while there is no base Dockerfile and no `--image`, fails
with a sentence saying which.

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

Whether it is built from the Dockerfiles or by you, it is any glibc-based image that has `/bin/sh`, with what the project's runs need: its language and package manager, its
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

A run never pulls (`--pull never`): an image of `--image` or `--repo-image` is in Podman's storage before the server
starts, or the plugin does not start. Building a new image under the same name takes effect at the next run. The same
Dockerfile written on the repository's page or the Settings page needs none of this: the plugin builds it.

## Platforms and checks at start

The plugin checks once, when it starts, and exits with a sentence (in `<data_dir>/plugins/<name>.log`) when it cannot
sandbox: a launcher that is not running fails every run, so a run never starts outside a container.

- Linux only, where rootless Podman needs no virtual machine. Other platforms are refused.
- `podman info` must answer, and say that Podman is rootless (a container of root's Podman would not be the user's).
- Podman 4.4 or later, the first that passes pasta's options through, and pasta (the package `passt`), which Podman 5
  names in `podman info` and Podman 4 finds on the `PATH`.
- Every image of `--image` and `--repo-image` exists (`podman image exists`). Neither is required, since each repository
  may bring a Dockerfile.

The wrapped Podman, the stop command and `leftovers` get the plugin's own `PATH`, `HOME`, `XDG_RUNTIME_DIR`,
`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `DBUS_SESSION_BUS_ADDRESS`, `CONTAINERS_CONF`, `CONTAINERS_STORAGE_CONF` and
`CONTAINER_HOST`, so that they find the storage the checks saw.

## Known limits

- The clone is read only, so git commands in the worktree that write the clone's git directory (the worktree's index)
  fail; Moebius commits from outside the sandbox.
- From a Moebius whose launch requests have no `repository` field, the repository is told by its clone's directory
  name, and there are no Dockerfiles.
- Every container shares the host's kernel. A stronger boundary is a runtime with its own kernel (gVisor's `runsc`, or
  `krun` for a micro-VM per run), which this plugin does not set yet.
- Only Pi-Bolt is launched through a launcher today.

## Installing

See the [README of this repository](../../README.md#installing-a-plugin-by-hand). Podman and pasta are installed
separately (`dnf install podman passt`, `apt install podman passt`).

## Tests

`cargo test -p moebius-plugin-podman-sandbox` runs the unit tests and `tests/protocol.rs`. The unit tests cover the
container's arguments, the settings, the image chosen from the Dockerfiles, the checks against a fake Podman, the builds
of missing images (the base first), the launch and the leftovers. The protocol test
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
