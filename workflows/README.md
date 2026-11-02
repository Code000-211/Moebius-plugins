# Workflow packages

A workflow package is a set of column scripts (`.luau`) for a repository's `.moebius/` folder. `moebius pkg install`
proposes the files as a change that a person merges. There are none yet.

A package is a folder named like the package, with a manifest and its scripts:

```
workflows/<name>/
├── moebius-package.toml
└── <script>.luau
```

```toml
name = "<name>"
version = "1.0.0"
kind = "workflow"
description = "One sentence."
moebius = ">=0.2"

[[files]]
source = "<script>.luau"   # the file in this folder
to = "<script>.luau"       # where it goes: .moebius/<to> of the repository, ending in .luau
```

The manifest is read as described in `docs/reference/packages.md` of the Moebius repository. A release is the tag
`<name>-v<version>`; the index names each file at that tag with its sha256, so a released file never changes.
