# docs

## Purpose

Serve the NixFleet documentation locally using mdbook.

## Location

- `modules/apps.nix` (the `docs` app definition)

## Usage

```sh
nix run .#docs
```

Opens the technical reference documentation at `http://localhost:3000`.

## How it works

Runs `mdbook serve --open` from the `docs/src/` directory. Opens a browser automatically. Press `Ctrl-C` to stop.

## Links

- [Apps Overview](README.md)
