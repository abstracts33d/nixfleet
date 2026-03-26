# Dev Tools

The development environment activated by `isDev = true`.

## What You Get

The dev scope provides a complete development environment:

- **Language managers** — mise for runtime versions (Node.js, Python, etc.)
- **direnv** — automatic environment loading per project
- **Docker** — containerization (NixOS only)
- **Build tools** — gcc, make, cmake, pkg-config
- **Claude Code** — AI-assisted development (configured via Home Manager)

## Per-Project Environments

With direnv and Nix flakes, each project gets its own isolated environment:

```sh
# In any project with a flake.nix
cd my-project    # direnv auto-loads the devShell
```

No global package pollution. No version conflicts.

## mise for Runtimes

For languages that need specific runtime versions (Node.js, Python, Ruby), mise handles version management declaratively.

## Docker

Docker is enabled automatically on NixOS dev hosts. Docker data is persisted on impermanent systems.

On macOS, use Docker Desktop or a Lima VM instead (not managed by this config).

## Further Reading

- [Claude Code Integration](claude.md) — AI tooling setup
- [Technical Dev Scope Details](../../scopes/dev.md) — packages and options
