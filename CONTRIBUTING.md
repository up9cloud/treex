# Contributing to treex

## Getting it running

```sh
make dev                              # TUI + web view on this repo
make dev DIR=~/src                    # somewhere else
make dev WEB=0                        # TUI only
make dev TUI=0 HOST=0.0.0.0           # headless, reachable on the LAN
make help                             # every parameter and its current value
```

`make dev` builds with the default features, which is exactly what a stock
`cargo install treex` gets.

## Checks

```sh
make test          # the Rust suite
make page-test     # drives the real browser page (needs node)
make lint          # rustfmt, clippy and the filename check, as CI runs them
make check-all     # every feature combination compiles on its own
```

All four run in CI (`.github/workflows/main.yml` — one workflow, with the
release jobs gated on `github.ref_type == 'tag'` so a tag cannot skip them). `make check-all` is not busywork: each feature is
advertised as optional, so each has to build alone, and it has caught the
binary failing to compile without `web` more than once.

### `make page-test`

`tool/page-test.mjs` pulls the `<script>` out of `src/web/assets/index.html`,
runs it in a minimal DOM against a real `treex` server, and clicks things.

It exists because the page can break in total silence. The snapshot fields are
read by name in JavaScript, so renaming one in Rust does not fail to compile —
every field simply arrives as `undefined`, every row quietly becomes a
non-directory, and no Rust test notices. That has happened twice.

If you add an element to the page you do not need to touch the harness: unknown
ids are created on demand.

## Layout

| | |
|---|---|
| `src/tree.rs` | the model: arena, expansion state, reconciling refreshes |
| `src/scan.rs` | reading one directory level |
| `src/state.rs` | `Session` — the one place several views meet |
| `src/preview.rs` | reading a file under a size limit |
| `src/tui/` | the ratatui view; `hit.rs` resolves mouse clicks |
| `src/web/http.rs` | a minimal HTTP/1.1 server — there is no axum or hyper |
| `src/web/mod.rs` | routing, the WebSocket protocol, and the single-page frontend |
| | `POST /rpc` is the only question endpoint; `/f/<path>` serves files raw |
| `src/watch.rs` | `notify`, watching only the directories on screen |

Three things are easy to break without noticing:

- **A filename Windows cannot represent breaks `git clone` for every Windows
  user**, and it fails during checkout with nothing useful to point at. `make
  lint` runs `tool/check-filenames.mjs`; a sample file called
  `hash#and?query.txt` is how we found out.
- **Paths from `Tree` are canonical, and the path you opened it with may not
  be.** On macOS a temporary directory under `/var` resolves to `/private/var`,
  so looking a node up by the path you passed to `Tree::new` finds nothing —
  while passing on Linux. Use `root_path()`.

- **`render.rs` and `hit.rs` share `INDENT` and `TWISTIE_WIDTH`.** They are how
  a screen column becomes a tree node. Change one without the other and clicks
  land on the wrong row, silently.
- **`Row` is serialized positionally.** `[depth, name, kind, flags]`, no field
  names and no path — the client rebuilds paths from depth-first order. Changing
  the order or the flag bits silently breaks the page; `rows_are_positional_and_carry_no_path`
  in `tests/sync.rs` is what catches it, and `tool/page-test.mjs` exercises the
  real decoder.
- **Structural changes must go through `Tree::reshaped()`.** The web protocol
  sends the whole tree only when `shape` moves, so a mutation that changes
  `rows()` while bumping `revision` alone leaves every browser stale. Cursor
  moves are the only things that bump `revision` on its own.

## Conventions

- en-US spelling everywhere, including comments and log strings.
- Comments earn their place by saying something the code cannot. No narration.
- `cargo fmt` before pushing; CI fails on a diff.

## Releasing

Tagging `v*` runs every check, builds binaries for five targets, cuts a GitHub
release and publishes to crates.io. Do not tag without checking the two things
no test covers: the mouse in a real terminal (and inside tmux), and the web view
from an actual phone.
