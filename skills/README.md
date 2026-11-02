# Skills packages

A skills package is skills for the harness configuration repository, proposed as a change under its `skills/`. There
are none yet.

```
skills/<name>/
├── moebius-package.toml
└── <skill>/SKILL.md
```

```toml
name = "<name>"
version = "1.0.0"
kind = "skills"
description = "One sentence."
moebius = ">=0.2"

[[files]]
source = "tdd/SKILL.md"
to = "tdd/SKILL.md"    # lands at skills/tdd/SKILL.md, the skill "tdd"
```

A release is the tag `<name>-v<version>`; see `docs/reference/packages.md` of the Moebius repository.
