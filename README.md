<img src="https://raw.githubusercontent.com/up9cloud/treex/master/assets/logo.svg" alt="" width="72" align="left">

# treex

An interactive directory tree for the terminal — and the same tree in your
browser, live-synced.

<br clear="left">

[![crates.io](https://img.shields.io/crates/v/treex.svg)](https://crates.io/crates/treex)
[![docs.rs](https://img.shields.io/docsrs/treex)](https://docs.rs/treex)
[![CI](https://github.com/up9cloud/treex/actions/workflows/main.yml/badge.svg)](https://github.com/up9cloud/treex/actions/workflows/main.yml)

<img src="https://raw.githubusercontent.com/up9cloud/treex/master/assets/screenshot.png" alt="treex in a browser: the tree on the left with the directories git ignores dimmed, and Cargo.toml open and coloured on the right" width="720">

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
treex --no-highlight                        # do not color file contents
treex --no-git-ignore                       # do not dim what git ignores
```

| Key | |
|---|---|
| `↑` `↓` / `k` `j` | move the cursor, which shows the file it lands on — or scroll the file, when it has the keyboard |
| `→` / `l` | expand a directory, or open a file and go to it |
| `←` / `h` | collapse, else go to the parent — or, from the file, put the keyboard back in the tree |
| `Enter` `Space` | toggle a directory, or open a file and go to it |
| `Tab` | move between the tree and the file |
| `c` | clear the way for a selection, or put everything back |
| `b` | fold the tree away, or bring it back — same as clicking `«` / `»` |
| `#` | show or hide the file's line numbers |
| `,` | show or hide space, tab and line-ending marks |
| `m` | give the mouse back to the terminal, or take it |
| left click | show a file; on a directory, toggle it |
| drag the divider | change how the two panes share the screen |
| `g` `G` | top / bottom |
| `PgUp` `PgDn` / `Ctrl-u` `Ctrl-d` | a screenful either way |
| `2` `3` `E` | expand to 2, 3, all levels |
| `z` | collapse all |
| `r` | refresh |
| `.` | show / hide dotfiles |
| `q` | quit |

Dotfiles are **shown by default** — `.github/`, `.env` and `.gitignore` are
things you opened a source tree to see. `.` in the terminal and the `hide .*`
button in the browser are the same switch, so both sides change together.

## Reading a file

The cursor shows whatever it can. One click or one arrow key onto a file
displays it — there is no second step — and the terminal splits: tree on the
left, file on the right.

A directory has nothing of its own to show, so **the file you were reading
stays up** while you move around the tree. That is the one case where the
cursor and the pane are on different things; touching the file again — scroll
it, click it — brings the cursor back to it.

`Enter` and `→` mean *open this*, so they hand the keyboard to the file:
`↑↓`, `PgUp`/`PgDn` and `g`/`G` then scroll it, and the cursor in the tree
turns purple to say so. The cursor landing on a file
shows it without taking the keyboard away, so you can keep arrowing through the
tree. `Tab` moves between the two panes, and so does `←` from the file — it
hands the keyboard back without putting the file away, since something is
always on display. The footer says which `←` you have: `← tree` from the file,
`← up` from the tree.

### Selecting and copying

treex never touches your clipboard — your terminal already knows how, and it is
the only thing that works over SSH, through a multiplexer and on every terminal
there is. What treex does is get out of the way.

**`c` does all of it**: folds the tree, hides the line numbers and the
whitespace marks, and hands the mouse back. Press it again and everything returns to where it was. Select
however you normally would and copy with `Cmd-C`, `Ctrl-Shift-C` or your
terminal's menu.

The four pieces are also separate keys. The footer says which state each one
is in rather than what pressing it would do, and **lit means on**:

| | | |
|---|---|---|
| `#` | `Line#` | lit: the numbers are showing |
| `,` | `·→` | lit: the whitespace marks are showing |
| `b` | `«` `»` | lit `«`: the tree is showing; dim `»`: it is folded away |
| `m` | `mouse` | lit: treex has the mouse; dim: your terminal has it back |

Every one of them is also a button — clicking `# Line#` in the footer is the
same as pressing `#` — as is the `«` / `»` on the file's own header. (Once you
have clicked `m mouse`, of course, the mouse is your terminal's: press `m` to
take it back.)

Each of them matters. An ordinary terminal selection runs from one line into
the next, so line numbers left on screen are dragged in along with the code,
and a border is picked up at both ends of every line. A `·` drawn for a space
would be copied as a `·`. And while treex is holding the
mouse, a drag is treex's, not your terminal's. (`--no-mouse` starts that way,
and most terminals let you hold `Shift` to bypass it — `Fn` in macOS Terminal.)

Long lines are still clipped at the right edge, and a clipped tail cannot be
selected — that one needs the file's own URL under `/f/`, or the browser.

The mouse wheel scrolls whichever pane it is over, and the divider between them
drags: grab either of its two lines and the panes follow, down to 26 columns of
tree and 46 of file. The width lasts as long as the session — closing the file
and opening another keeps it — but treex persists nothing between runs, so a
new one starts at the default share again.

Below 72 columns there is no room for both, so the file takes the window —
widen it and the tree is back. (`b` folds the tree away at any width.)

### Both views follow one scroll position

Where the file is scrolled to is shared as well: scroll in the terminal and the
browser follows, and the other way round. A window too short to reach the line
sits at its own end rather than refusing to follow, and whichever side scrolled
last is the one being followed. Turning `wrap` on in the browser opts that tab
out — a wrapped line is several rows, so there is no line to agree on.

### Whitespace is visible by default

Spaces are `·`, tabs are `→`, and line endings are `␊` — with a `␍` in front of
it where the file has one. Trailing whitespace and a stray carriage return are
the kind of thing a file viewer should not be hiding, so this starts on; `,`
turns it off, and `c` turns it off along with everything else that would end up
in a selection.

### Syntax coloring uses your `bat`

If [`bat`](https://github.com/sharkdp/bat) is on your `PATH`, treex asks it to
color the file and draws the result — in the terminal *and* in the browser.
That means your own `bat` theme, `--map-syntax` rules and config file are what
you see, and `BAT_THEME=ansi treex` works the way you would expect. Debian's
`batcat` is found too.

No `bat`, and files are shown as plain text. There is no highlighting engine
compiled into treex: it is a few hundred lines of ANSI parsing against a
program you either have or do not, and `--no-highlight` turns it off.

In the browser the file gets a backdrop to match the theme's — `bat` assumes a
terminal whose background its colors were chosen against, and a web page has
its own.

Files over **256 KiB** are shown uncolored — `bat` takes most of a second on a
megabyte, and the browser would have to draw a quarter of a million spans.

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

The browser is laid out the same way: tree on the left, file on the right,
colored the same way the terminal colors it, with line numbers, a wrap toggle
and font-size controls. The **`«` button beside the file's name folds the tree
away**, and turns into `»` to bring it back — the same control the terminal
has, in the same place. On a phone there is only room for one, so the file takes the
window and `←` returns to the tree. Only files currently in the tree can be read — being in the
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

All three are on by default — a stock `cargo install treex` is about 1.4 MB and
has everything. The switches are there for library users, who can take
`default-features = false` and get the model on its own.

A directory contributes at most 5,000 entries to the tree; the rest are
reported as `… N more` on the directory's own line. Nobody scrolls a hundred
thousand rows, and offering to would cost every view dearly. The browser
renders only the rows on screen, so scrolling a large tree stays cheap, and
anything large is deflated before it leaves — this repo fully expanded is 64 KB
on the wire and a keypress is 59 bytes.

## What git ignores is dimmed

If `git` is on the machine, treex asks it what it would ignore and draws those
rows dimmed — `target/`, `node_modules/`, your `.env` — along with `.git`
itself. **Nothing is hidden**: they are still there to open, still counted,
still searchable by eye. Dimming says "not what you came for"; hiding would be
a different feature with a different argument.

git does the deciding, so nested `.gitignore` files, negations,
`core.excludesFile` and `.git/info/exclude` all behave exactly as they do in
git, and a folder holding a dozen unrelated checkouts needs no configuration —
each directory is judged by whatever repository it belongs to. A file that is
already tracked is never dimmed, whatever the patterns say.

No git, or a directory in no repository, and nothing is dimmed.
`--no-git-ignore` turns it off.

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
