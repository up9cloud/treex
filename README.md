<img src="https://raw.githubusercontent.com/up9cloud/treex/master/assets/logo.svg" alt="" width="72" align="left">

# treex

An interactive directory tree for the terminal — and the same tree in your
browser, live-synced.

<br clear="left">

[![crates.io](https://img.shields.io/crates/v/treex.svg)](https://crates.io/crates/treex)
[![docs.rs](https://img.shields.io/docsrs/treex)](https://docs.rs/treex)
[![CI](https://github.com/up9cloud/treex/actions/workflows/main.yml/badge.svg)](https://github.com/up9cloud/treex/actions/workflows/main.yml)

<img src="https://raw.githubusercontent.com/up9cloud/treex/master/assets/screenshot.png" alt="treex in a browser: the directory tree with src/, tui/ and web/ expanded" width="720">

Most terminal file managers show you *one directory at a time* (ranger, yazi,
nnn) or *fit the tree to your screen by hiding branches* (broot). `treex` does
neither. It behaves like the sidebar in VS Code or the list view in Finder: what
you expand stays expanded, the tree scrolls, and the filesystem is watched so
things appear and disappear as they happen.

Then it does one more thing: `--web` serves that same tree over HTTP, and the
terminal and the browser are one session. Open a file on your phone and the
terminal's cursor moves to it.

> Status: pre-1.0. The shape is here and it is tested, but nothing is stable yet.

## Install

```sh
cargo install treex
```

## Use

```sh
treex                       # browse the current directory
treex ~/src -L 3            # start with three levels expanded
treex -p > structure.txt    # print instead; also what happens when stdout is a pipe
treex --web                 # ...and serve it at http://localhost:11711
treex --web 0.0.0.0:11711 --no-tui          # headless, reachable on the network
treex --web --max-preview-size 4m           # bigger files readable in the browser
treex --web --no-preview                    # tree only, no file contents
```

| Key | |
|---|---|
| `↑` `↓` / `k` `j` | move the cursor |
| `→` / `l` | expand a directory, or open a file for reading |
| `←` / `h` | stop reading, else collapse, else go to the parent |
| `Enter` `Space` | toggle a directory, or open a file |
| left click | move the cursor; on a directory, toggle it |
| double click | open a file for reading |
| right click | open a file for reading |
| `g` `G` | top / bottom |
| `2` `3` `E` | expand to 2, 3, all levels |
| `z` | collapse all |
| `r` | refresh |
| `.` | show / hide dotfiles |
| `q` | quit |

Dotfiles are **shown by default** — `.github/`, `.env` and `.gitignore` are
things you opened a source tree to see. `.` in the terminal and the `hide .*`
button in the browser are the same switch, so both sides change together.

Reading a file is a second step on top of the cursor. Arrow keys move the
cursor and leave whatever was open; `→`, `Enter` or a double click opens; `←`
closes. A single click only moves the cursor — otherwise browsing with the
mouse would load every file you passed over. The
cursor is drawn in a different color while a file is open, so the terminal
always shows what the browser is displaying.

## Reading the tree from a phone

This is what `--web` is for. On the machine holding the files:

```sh
treex --web 0.0.0.0:11711 --no-tui ~/project
```

Then open the URL it prints on the tablet. The page is plain HTML with a
WebSocket — no app, no build step, works in Safari on iOS.

`--web` binds `127.0.0.1` unless you say otherwise, which means **nothing
outside this machine can reach it** — including a phone on the same VPN. Use
`0.0.0.0` for that, and treex prints the address other devices can actually
use:

```console
treex /home/you/project
  http://127.0.0.1:11711
  http://localhost:11711
```

The port defaults to **11711** and steps forward to 11712, 11713 and so on if it
is taken, so a second treex comes up next door rather than refusing to start.

Clicking a file shows its contents, with line numbers, a wrap toggle and
font-size controls. Only files currently in the tree can be read — being in the
tree already means the path is under the root and passed the hidden and ignore
rules, so there is no separate traversal check to forget. Files over
`--max-preview-size` (**1 MiB** by default) report their size instead of their
contents, and binary files are refused rather than dumped. `--no-preview` turns
the whole thing off.

Every visible file is also served under `/f/` — `README.md` is at
`http://localhost:11711/f/README.md` — and the ↗ button in the floating group
opens it in a new tab. That is the way to look at an image, a PDF, or something
too large for the preview pane: the browser renders it natively and the file is
streamed rather than read into memory. The prefix keeps a file that happens to
be called `ws` or `api` reachable.

Files are served with their real content type, so the browser decides what to
do: Markdown, JSON and images render, a CSV goes to whatever opens spreadsheets.
Every response is sandboxed, so an HTML or SVG file in the tree renders without
being able to script against treex itself.

### It has no authentication

`--web` defaults to `127.0.0.1` for that reason. treex never writes to your
files, so the exposure is disclosure rather than damage — but anyone who can
reach the port can read your whole directory structure and the contents of any
file in it. Put it behind Tailscale, a reverse proxy with auth, or an SSH
tunnel:

```sh
ssh -L 11711:localhost:11711 you@host   # then --web stays on 127.0.0.1
```

## Mouse support

Mouse is on by default in the TUI. Clicking the `▸`/`▾` marker toggles a
directory; clicking its name does too, which is what VS Code does — pass
`--no-click-toggle` if you would rather only the marker did.

Two things worth knowing:

- **Capturing the mouse takes over text selection.** Hold `Shift` while dragging
  to get your terminal's native selection back, or run with `--no-mouse`.
- **Under tmux you need `set -g mouse on`,** otherwise tmux keeps the events.

## Build features

| Feature | Default | |
|---|---|---|
| `tui` | yes | the terminal view and its mouse handling |
| `watch` | yes | reacts to filesystem changes, watching only the directories you have expanded |
| `web` | yes | the HTTP server and the browser page |

All three are on by default — a stock `cargo install treex` is about 1.6 MB and
has everything. The switches are there for library users, who can take
`default-features = false` and get the model on its own.

A directory contributes at most 5,000 entries to the tree; the rest are
reported as `… N more` on the directory's own line. Nobody scrolls a hundred
thousand rows, and offering to would cost every view dearly. The browser
renders only the rows on screen, so scrolling a large tree stays cheap, and
anything large is deflated before it leaves — this repo fully expanded is 64 KB
on the wire and a keypress is 59 bytes.

treex does not read `.gitignore`. A Rust or Node checkout will show `target/`
and `node_modules/`; collapse them and move on. Filtering by project convention
is a different job from browsing a directory, and it cost more than the web
server did.

## As a library

The binary is a thin wrapper. `Tree` is the model, `Session` lets several views
drive one tree, and both bundled views are optional features.

```sh
cargo add treex --no-default-features
```

See **[the API documentation](https://docs.rs/treex)** — it is the reference
for `Tree`, `Session`, `Command`, `Snapshot` and `Row`, with runnable examples.

## Contributing

See [CONTRIBUTING.md](https://github.com/up9cloud/treex/blob/master/CONTRIBUTING.md).

## License

MIT.
