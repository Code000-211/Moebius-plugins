# Rules packages

A rules package is a fragment of a `rules.md`: its `## name` sections are proposed to be added to a repository's
`.moebius/rules.md`, as a change that a person merges. There are none yet.

```
rules/<name>/
├── moebius-package.toml
└── rules.md
```

```toml
name = "<name>"
version = "1.0.0"
kind = "rules"
description = "One sentence."
moebius = ">=0.2"

[[files]]          # exactly one file
source = "rules.md"
to = "rules.md"    # not used for a rules package
```

A release is the tag `<name>-v<version>`; see `docs/reference/packages.md` of the Moebius repository.
