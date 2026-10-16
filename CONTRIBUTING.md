# Contributing to treex

## Getting it running

```sh
make dev                              # TUI only, on this repo
make dev2                             # TUI + web view, reachable from other devices
make dev WEB=1                        # TUI + web view on 127.0.0.1 only
make dev DIR=~/src                    # somewhere else
make dev2 TUI=0                       # headless
make help                             # every parameter and its current value
```

`make dev` is the terminal alone. `make dev2` adds the web view bound to `0.0.0.0`, for opening the page from a phone or over a VPN; `make dev WEB=1` keeps it on `127.0.0.1`, like an installed treex does. Both take the same parameters.

`make dev` builds with the default features, which is exactly what a stock `cargo install treex` gets.

## Checks

```sh
make test          # the Rust suite
make page-test     # drives the real browser page (needs node)
make lint          # rustfmt, clippy and the filename check, as CI runs them
make check-all     # every feature combination compiles on its own
make windows       # type-check for Windows without pushing to find out
```

All four run in CI (`.github/workflows/main.yml` — one workflow, with the release jobs gated on `github.ref_type == 'tag'` so a tag cannot skip them). `make check-all` is not busywork: each feature is advertised as optional, so each has to build alone, and it has caught the binary failing to compile without `web` more than once.

Coloring needs `bat` on the machine, and the dimming tests need `git`. The tests that use it return early without one rather than failing, so a box with no `bat` still runs green — and proves less. Install `bat` before trusting a green run on anything about colors.

### `make page-test`

`tool/page-test.mjs` pulls the `<script>` out of `src/web/assets/index.html`, runs it in a minimal DOM against a real `treex` server, and clicks things.

It exists because the page can break in total silence. The snapshot fields are read by name in JavaScript, so renaming one in Rust does not fail to compile — every field simply arrives as `undefined`, every row quietly becomes a non-directory, and no Rust test notices. That has happened twice.

If you add an element to the page you do not need to touch the harness: unknown ids are created on demand.

## Layout

| | |
|---|---|
| `src/tree.rs` | the model: arena, expansion state, reconciling refreshes |
| `src/scan.rs` | reading one directory level |
| `src/state.rs` | `Session` — the one place several views meet |
| `src/preview.rs` | reading a file under a size limit |
| `src/highlight.rs` | coloring it, by running the user's `bat` |
| `src/git.rs` | what git ignores, by running `git check-ignore` |
| `src/tui/` | the ratatui view; `hit.rs` resolves mouse clicks, `viewer.rs` is the file pane |
| `src/web/http.rs` | a minimal HTTP/1.1 server — there is no axum or hyper |
| `src/web/mod.rs` | routing, the WebSocket protocol, and the single-page frontend |
| | `POST /rpc` is the only question endpoint; `/f/<path>` serves files raw |
| `src/watch.rs` | `notify`, watching only the directories on screen |

Three things are easy to break without noticing:

- **Filesystem events are not equally prompt across platforms.** macOS uses FSEvents, where registering a watch restarts the stream; a test that writes a file the instant a directory is watched is testing the platform, not treex.
- **Anything unix-only needs `#[cfg(unix)]`, tests included.** `std::os::unix::fs::symlink` in a test is a compile error on Windows, not a skipped test. `make windows` catches that here rather than three CI minutes later.
- **A filename Windows cannot represent breaks `git clone` for every Windows user**, and it fails during checkout with nothing useful to point at. `make lint` runs `tool/check-filenames.mjs`; a sample file called `hash#and?query.txt` is how we found out.
- **Paths from `Tree` are canonical, and the path you opened it with may not be.** On macOS a temporary directory under `/var` resolves to `/private/var`, so looking a node up by the path you passed to `Tree::new` finds nothing — while passing on Linux. Use `root_path()`.

- **`render.rs` and `hit.rs` share `INDENT` and `TWISTIE_WIDTH`.** They are how a screen column becomes a tree node. Change one without the other and clicks land on the wrong row, silently.
- **`Row` is serialized positionally.** `[depth, name, kind, flags]`, no field names and no path — the client rebuilds paths from depth-first order. Changing the order or the flag bits silently breaks the page; `rows_are_positional_and_carry_no_path` in `tests/sync.rs` is what catches it, and `tool/page-test.mjs` exercises the real decoder.
- **Structural changes must go through `Tree::reshaped()`.** The web protocol sends the whole tree only when `shape` moves, so a mutation that changes `rows()` while bumping `revision` alone leaves every browser stale. Cursor moves are the only things that bump `revision` on its own.

## Conventions

- en-US spelling everywhere, including comments and log strings.
- Comments earn their place by saying something the code cannot. No narration.
- `cargo fmt` before pushing; CI fails on a diff.
- Markdown is not hard-wrapped: one line per paragraph or list item. The long lines are on purpose: they are what exercises wrapping in both viewers.

## Releasing

A `v*` tag runs every check — fmt, clippy, MSRV, three OSes, the eight feature combinations, the browser page — then checks the tag against `Cargo.toml`, builds binaries for five targets, cuts a GitHub release, publishes to crates.io and pings Telegram.

**Pushing the tag is the point of no return**: a published version can be yanked but never re-uploaded. Everything before it can still be undone.

At `0.x` a breaking change bumps the minor and everything else, features included, bumps the patch.

### Before the tag: what no test covers

Build it (`make dev2`) and go through these by hand:

- **The terminal, in a real one and inside tmux** (`set -g mouse on`): clicking rows and twisties, dragging the divider, the wheel over each pane, the four scroll buttons on the file's right border and that they dim at either end, every footer switch. The keys `b` `#` `,` `w` `m` `c`, and a selection after `c`.
- **The file pane**: a long line wraps and unwraps with `w`; a file written while it is open shows the new contents in both views; with `chafa` installed, a picture draws and zooms.
- **The web view from an actual phone**: open a file, scroll it, and check the terminal follows; play a video, in Safari too.
- **Any new character the terminal draws, on a Mac.** No macOS font has `⭱` `⭳`, so they were boxes there and nowhere else.

### Steps

1. If what either view looks like has changed, retake the screenshots. It needs a Chromium reachable over CDP and `playwright-core`, which downloads no browser:

   ```sh
   npm install --prefix /tmp/shot playwright-core
   cargo build
   NODE_PATH=/tmp/shot/node_modules node tool/screenshot.mjs <cdp-url> <this-host>
   ```

   `<this-host>` is this machine's address as that browser sees it.
2. Bump `version` in `Cargo.toml`, and the `treex` entry in `Cargo.lock` **by hand**. Letting cargo update the lock can rewrite its header from `version = 3` to `version = 4`, an unrelated change that would ride along in the release commit; check `head -3 Cargo.lock`.
3. `cargo build --locked` and `cargo publish --dry-run --locked`.
4. Commit as `Bump to X.Y.Z`, push `master`, and wait for CI to pass on it.
5. `git tag -a vX.Y.Z -m vX.Y.Z && git push origin vX.Y.Z`.
6. Watch the run. The Actions API answers without a token on a public repository:

   ```sh
   curl -s 'https://api.github.com/repos/up9cloud/treex/actions/runs?per_page=3'
   ```
