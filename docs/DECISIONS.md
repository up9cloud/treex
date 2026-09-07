# treex — design decisions

Why the code is the way it is. Not a reference: the API is rustdoc, the layout
and checks are in CONTRIBUTING.md, the CLI is in README.md. This file is only
the reasoning that the code cannot show — most entries record something that was
tried, measured, and rejected.

Read the relevant entries before changing the wire protocol, the tree model, the
watcher or the page's rendering. Several of them say, in effect, *this was built
three times; ask before building it a fourth.*

## Decisions

- **`Tree::new` strips Windows' verbatim prefix.** `canonicalize` returns
  `\\?\C:\...` there, and inside a verbatim path a forward slash is an
  ordinary character rather than a separator. The browser rebuilds every path by
  concatenating with `/`, so on Windows *nothing in the tree would open* —
  `by_path` would never match. std re-adds the prefix itself when a long path
  needs it, so dropping it from the representation costs nothing and also stops
  `\\?\C:\...` appearing in the header. `/f/<path>` builds its `PathBuf`
  segment by segment for the same reason. Pinned by
  `a_path_joined_the_way_the_browser_joins_it_finds_its_node`.

- **Every path in the repository has to be checkable-out on Windows.** A sample
  file called `hash#and?query.txt` made `git clone` fail for every Windows user,
  and the failure landed during checkout — before any job, with no log to read.
  `tool/check-filenames.mjs` runs in `check` and in `make lint`. `#`, `%`, `&`
  and `+` are fine; `< > : " | ? *`, reserved device names and trailing dots or
  spaces are not.
- **`Tree` canonicalises its root, so tests must not look nodes up by the path
  they passed in.** On macOS a temp directory under `/var` is really
  `/private/var`, and six tree tests passed on Linux purely because the two
  happen to be equal there. `fixture()` hands back the canonical root as a third
  value for that reason, and
  `a_root_reached_through_a_symlink_still_answers_by_its_own_paths` pins the
  behavior on any platform.

- **A directory contributes at most `ScanOptions::max_entries` rows** (5,000),
  with the remainder reported as `Row::omitted` and drawn as `… N more` on the
  directory's own line. This is the cheap answer to enormous directories: the
  expensive one — streaming the tree in chunks — only moves the cost around,
  because building a hundred thousand DOM nodes is the real limit however
  patiently they arrive.
- **The browser renders only the rows on screen.** Rows are absolutely
  positioned inside a spacer of the full height, with twelve rows of overscan.
  Consequences worth knowing: row height must stay fixed (`height`, not
  `min-height`) and is re-measured whenever the font size changes;
  `scrollIntoView` cannot be used because the target may not exist, so `reveal`
  computes `scrollTop` from the index instead.
- **`Session::cursor()` answers without flattening.** After the cursor/snapshot
  split the common case was building every `Row` only to read four numbers.
  `Tree::visible_index` walks the same depth-first order counting instead of
  cloning a name and a path per row.
- **Entries have a `Kind`, not two booleans.** `is_dir` + `is_symlink` could
  not express a socket, a fifo or a device, and made "symlink to a directory"
  ambiguous. `Kind` is what the entry resolves to; `symlink` is orthogonal and
  records how it was reached. A dangling link is `Kind::Broken`. All three views
  show `ls -F` suffixes, so a socket is never mistaken for a file.
- **Rows are serialized positionally and carry no path.**
  `[depth, name, packed]`, with a fourth element only when `omitted` is
  non-zero; `packed` is `kind << 2 | expanded | symlink << 1`. The path is reconstructible
  because rows are in depth-first order — every ancestor is the last row seen at
  each smaller depth — so sending it was the largest and most redundant field on
  the wire. Measured on this repo expanded four levels: **1470 KB -> 290 KB**,
  the same order gzip would have managed and with no compression dependency,
  less parsing and less client memory.
  `tests/sync.rs` has its own `decode` mirroring the page's, so the tests speak
  the protocol rather than asserting against a shape nobody sends.
- **No axum, no hyper: `src/web/http.rs` is the server.** 311 lines of HTTP/1.1
  against twenty crates under a tool whose job is reading your files — the
  motivation is supply chain, not size, though it is also 298 KB smaller.
  Nothing about framing is hand-rolled: `httparse` does request parsing (it was
  already here for the WebSocket handshake) and `tungstenite`'s
  `derive_accept_key` + `WebSocketStream::from_raw_socket` do the upgrade, so
  the 101 response is the only part written by hand. Keep-alive is supported;
  `Body::File` writes exactly the `Content-Length` promised so a short read
  cannot desync the connection. Caps on head size, body size and read time are
  there because nothing else is now.
  The integration tests were already raw HTTP over TCP, which is why the swap
  landed with all eighteen passing unchanged — that was worth more than any
  amount of care while writing it.
- **Paths are `Arc<Path>`, shared between `Node` and the `by_path` index.**
  They used to be stored twice, and on this repo an absolute path averages 118
  bytes — a third of the tree's memory went on the second copy. Measured on
  18,061 rows: **15.4 MB RSS -> 9.9 MB**. `Command` still owns `PathBuf`,
  because it is deserialized from JSON and `Arc<Path>` cannot be; the TUI pays
  one `to_path_buf` per keypress, which is nothing.
- **A newly watched directory is re-read straight after registering.** A
  directory is drawn before it is watched, and on macOS registering a watch
  restarts the FSEvents stream, so anything happening in that window would be
  missed for good — there is no later event to recover it. `sync_watches`
  refreshes whatever it just started watching.
- **`watch()` registers the first watch before it returns.** It used to spawn a
  thread that did it, so anything happening in the first few milliseconds was
  missed for good — a race that showed up as a flaky test only under load, but
  which at startup is exactly when files move.
- **Compression is done a layer above the WebSocket, because tungstenite has
  none.** `permessage-deflate` appears in tungstenite 0.29 only as a test
  fixture for header parsing — there is no implementation, and axum sits on
  tungstenite, so swapping axum out would not have unlocked it either. Instead
  the server deflates any message over 4 KB and sends it as a **binary** frame;
  the page tells the two apart by frame type and undoes it with the browser's
  own `DecompressionStream("deflate-raw")` — no library, no negotiation.
  Measured: **263 KB -> 64 KB**. Cursor messages stay text; at 59 bytes
  deflate would only add to them. HTTP previews use gzip instead, since
  `deflate` over HTTP means something browsers disagree about.
  Doing it ourselves also means only the messages worth it pay for it.
  Cost: `flate2` with the pure-Rust backend, +107 KB of binary.
- **The wire has two message kinds, because a keypress must not cost a
  megabyte.** `Tree` carries `revision` (any change) and `shape` (only when
  `rows()` would differ); moving the cursor bumps the first and not the second.
  The server sends a full `snapshot` when `shape` moved and a 59-byte `cursor`
  otherwise. Measured on this repo expanded four levels — 6937 rows — an arrow
  key went from **1470 KB to 59 bytes**. The page reuses its row elements on a
  cursor message instead of rebuilding thousands of nodes.
  Every structural mutation must go through `Tree::reshaped()`; bumping
  `revision` alone on something that changes the rows would leave every browser
  showing a stale tree.
- **`Tree::new` rejects anything that is not a directory.** `treex README.md`
  used to print one line and exit 0, which looks like success. This is what
  `error.rs` is for — it had been written and never used, with `thiserror`
  carried along for nothing.

- **Views address nodes by path, not `NodeId`.** Ids are recycled when a refresh
  frees a subtree, and a browser tab always acts on a snapshot that may already
  be stale. Paths make a late command a no-op instead of a wrong hit.
- **`Row` is the one projection.** The TUI, the web page and mouse hit-testing
  all derive from it; its index *is* the screen row. `render.rs` and `hit.rs`
  share `INDENT`/`TWISTIE_WIDTH` for this reason — changing one without the
  other makes clicks land on the wrong thing.
- **Bound to 127.0.0.1 by default, and it announces reachable addresses.**
  Binding `0.0.0.0` prints the default-route address first, found with a UDP
  `connect` that sends no packets. Printing only `localhost` there answered the
  wrong question — that flag is used precisely to reach treex from elsewhere.
  A non-loopback bind without `--web-read-only` warns about the missing auth.
- **File previews are conditional.** A request carries the `Stamp` (size and
  mtime) the client already holds; matching it returns `{"status":"unchanged"}`
  in twenty-two bytes instead of the file. The browser paints its cached copy
  immediately and revalidates behind it, so a cache entry cannot be stale by the
  time you look at it.
- **The browser caches file contents for five minutes.** Flipping between two
  files should not re-fetch either. The TTL is only a memory bound now that
  every request revalidates by stamp; correctness does not depend on it.
- **The file viewer's gutter is one `<pre>`, and it steps aside when wrapping.**
  Two columns cannot follow wrapped lines, so `wrap` hides the numbers and
  disables the `#` button rather than letting them drift. `align-items: stretch`
  keeps its background filling the column; `flex-start` left blank below the
  last number. A trailing newline is not another line — `lineCount` accounts for
  it, otherwise every file gets a phantom final number.
- **Both scrollers set `overscroll-behavior: contain`.** Without it, scrolling
  past the end of the file viewer chains into the tree behind it, which on iOS
  reads as the content sliding away into blank space.
- **The page has two font sizes.** `--fs` sizes the tree and file contents;
  `--ui` sizes the nav and never moves. Scaling the controls along with the
  content meant the buttons shrank out from under the finger adjusting them.
- **`viewing` lives in the shared session, not in the web module.** The terminal
  has no preview pane and does not want one, but it does show which file the
  browser is reading — so the state has to travel the same path as the
  selection. A reconcile that drops the file clears it, so neither view is left
  pointing at something gone.
- **The browser's `hide .*` is a filter, not a visibility switch.** Lit means
  the filter is engaged and dotfiles are hidden; unlit — the default — means
  everything is shown. The first version had it the other way round and read
  backwards.
- **The browser has no refresh button.** The watcher keeps the tree current, so
  it earned nothing; F5 re-renders from the server's state if a view ever looks
  stuck. `Command::Refresh` still exists for `r` in the TUI.
- **The logo is one SVG, served twice.** `assets/logo.svg` is the README mark
  and, via `include_str!` on a `/favicon.svg` route, the page's icon. It was
  briefly inlined as a percent-encoded data URI on the theory that the page
  makes no external requests — but same-origin is not external, and the data URI
  only made the SVG unreadable in the HTML and impossible to cache. It went
  through three drafts:
  connectors-only read as a coat rack, canopy-only read as broccoli; the shipped
  one keeps right-angle elbows visible below the canopy so it is a tree from
  across the room and a `tree` diagram up close. Checked at 16px on both light
  and dark, which is the only test that matters for a favicon.
- **`web` is a default feature.** It was opt-in on binary-size grounds, which
  was the wrong call: the browser view is the reason this project exists, and
  it costs 0.5 MB. Nobody should have to read the feature table to get the
  headline feature. The switches remain for library users, who take
  `default-features = false`.
- **There is no `.gitignore` support, on purpose.** It existed as an opt-in
  feature and was removed: hiding files by project convention is a different
  job from browsing a directory, and if the point were ever to present a tree
  to someone else it would need the user choosing what to hide, not git's
  answer. It was also the single most expensive thing in the binary. `target/`
  and `node_modules/` show; collapse them.
- **Dotfiles are shown by default**, in `ScanOptions::default()` and so for
  library users too. treex is for looking at source trees, where the dotfiles
  are half of what you came for. There is deliberately **no CLI flag** for it:
  both views toggle it live, so a startup override would only be a second way
  to say the same thing. The toggle is a `Command`, not a direct poke at
  `tree.opts`, so the browser can drive it and both sides stay in step.
- **`print_tree` writes through a `BufWriter` and stops on error.** `treex -p |
  head` used to panic out of `println!` on EPIPE. A closed pipe is a normal way
  for that command to end.
- **`--web-read-only` is gone.** It blocked the browser from sending commands,
  which in a single-user tool meant a web view that could not expand or open
  anything. It was also incomplete: `/api/file` never consulted it, so contents
  stayed readable to anyone who knew a path. One flag, one `WebOptions` field
  and one wire field removed.
- **Docs are split three ways.** README is user-facing only (CLI, keys, the
  phone case, security). The library API lives in `src/lib.rs` rustdoc as
  runnable doctests, and README links to docs.rs rather than restating it.
  Everything about working on treex is in CONTRIBUTING.md.
- **Double click opens a file; a single click still only selects.** A terminal
  reports presses, not gestures, so `Clicks` pairs them itself — same row,
  within 400 ms, and the pair is cleared on a match so three presses are one
  double and one single rather than two overlapping doubles. The clock is a
  parameter rather than `Instant::now()` inside, which is the only reason the
  timing rules have real tests. Verified with SGR mouse sequences sent through
  tmux, not just unit tests.
- **Reading a file is a second step on top of the cursor.** `selected` is where
  the cursor is; `viewing` is a file open for reading, and it is always the
  selected node or nothing. `Tree::select` clears `viewing` whenever the cursor
  actually moves, which is what makes `↑↓` a way out. `→`/`Enter`/right-click
  open; `←` closes. A click in the browser is one step — `Command::View` selects
  as well — because a browser has nothing corresponding to "merely highlighted".
  Two cursor colors, therefore: cursor only (bg 238) and reading (bg 91). Both
  set a foreground explicitly, since file rows carry no color of their own and a
  light terminal would otherwise put dark text on a dark background.
  This took three tries. Reporting `viewing` in the status bar was invisible;
  collapsing it into `selected` lost the two-step; letting it outlive cursor
  movement was an editor metaphor the user did not want. Ask before rebuilding
  this again.
- **Content types are accurate, not convenient.** `/f/` exists to hand a file to
  the browser and let it decide; reading something as plain text is what the
  preview pane is already for. So anything with a registered type gets it —
  JSON, YAML, TOML, Markdown, CSV, XML, CSS, JS, images, fonts, audio, video,
  archives — even where that means the browser downloads it instead of showing
  it. `text/plain` is left only for source code, logs and extensionless files,
  where it is the accurate answer rather than a fallback. An earlier version
  forced `.md` and `.toml` to plain text to keep them viewable in a tab; that
  was the wrong trade, because the viewable-in-a-tab case already has a home.
- **Every raw response carries `Content-Security-Policy: sandbox`.** HTML and
  SVG run scripts when opened as a top-level document, and served from treex's
  own origin they could drive the tree and read every visible file. The sandbox
  drops them into an opaque origin with scripting off, so HTML and SVG still
  render — they just cannot reach back.
- **Binary detection sniffs magic bytes as well as NUL.** A minimal PDF has no
  NUL anywhere and was being offered as text. `BINARY_MAGIC` in `preview.rs`
  covers PDF, ZIP, PNG, JPEG, GIF, ELF, gzip and friends. Content-based on
  purpose: a `.txt` that is really a PDF is still a PDF.
- **`handle_key` and `handle_mouse` return an `Action`, they do not apply it.**
  That is what let the event loop decide which commands go to `spawn_blocking`,
  and it made the keyboard testable without standing up a `Session` — the key
  semantics now have real unit tests instead of tmux screenshots.
- **HTTP has exactly one question endpoint: `POST /rpc`.** Commands already
  travel over the WebSocket, so a second vocabulary of REST-ish paths bought
  nothing — `/api/tree` and `/api/file` are gone, folded into `{"method":...}`.
  What remains outside it is not API: `/` and `/favicon.svg` are assets, and
  `/f/<path>` must stay a plain URL because a browser tab is what opens it.
- **Raw files live under `/f/`, not at the root.** Serving them at the root
  worked, because axum prefers static segments to a wildcard, but it meant a
  file genuinely called `ws` or `api` was unreachable. The prefix removes the
  question. Two guards, in order: any path component that is not `Normal` is a
  400 before the tree is consulted, then the same `visible_file` check as the
  preview. Unlike the preview there is no size limit — a link is allowed to
  point at something enormous — so the body is streamed in 64 KiB chunks with
  `futures_util::stream::unfold`, which needed no new dependency, only
  `tokio/fs` and `tokio/io-util`.
- **`/api/file` authorizes by tree membership, not by path comparison.** A file
  is readable exactly when it is a node in the tree, which already implies it is
  under the root and survived the hidden/ignore rules. A `starts_with(root)`
  check would have been a second thing to keep correct; this one cannot drift
  from what the user can see. Files inside collapsed directories are therefore
  404 until expanded, which is the intended behavior, not an accident.
- **The watcher tracks the drawn set, not the tree.** `notify` watches exactly
  the expanded directories, one level deep each, re-synced on every revision.
  On this repo that is 1 watch at rest against 2581 for a recursive watch of
  the root. The consequence is that a collapsed directory is unwatched and can
  go stale, which is why `Tree::expand` re-reads on the way open.
- **`refresh_path` never walks up past a directory it does not know.** It used
  to search for *some* loaded ancestor, so every write under `target/` re-read
  the root and redrew every view — 206 snapshots from 80 touches, and the
  revision counter spinning while idle. Anything outside the tree is now ignored
  outright, and `load_children` reports whether the visible listing actually
  changed so a touched mtime is not a redraw.
- **Port 11711, stepping forward on conflict.** Binding walks up to 20 ports and
  always announces the one it got, so a second treex never fails to start. This
  applies to an explicitly requested port too — the printed URL is what makes
  that safe rather than confusing.
- **Not a workspace.** One crate with `tui` / `watch` / `web` features. Split
  into a workspace only if the web view grows a build step.
- **`scan.rs` uses plain `std::fs::read_dir`.** No directory walker is needed
  because only one level is ever read. This is also what made dropping `ignore`
  cheap: gitignore matching was the only thing that wanted a walker.
- **A quiet socket still has to talk.** The server sends `alive` after ten
  silent seconds and the page gives up on one that has said nothing for
  twenty-five. A WebSocket ping cannot stand in: browsers answer those without
  telling the page, so only a frame the script can see proves the connection is
  real. Three places have to agree — `HEARTBEAT` in `web/mod.rs`, `SILENCE` in
  `index.html`, and the wait in `page-test.mjs`, which is why that test takes
  25 s rather than 12 s. Do not shorten the wait without moving the other two.
- **Reconnect backs off to five seconds, not ten.** A rebuild takes longer than
  the backoff needs to climb, so the old ceiling meant watching a dead page for
  up to ten seconds after treex was already back. Returning to the tab or to the
  network reconnects at once rather than serving out the rest of the wait —
  measured at 49 ms on a tab switch, against 2.6 s for the backoff alone.
- **The page shows both versions, `web:` and `sv:`.** The first is baked into
  the HTML when it is served, the second arrives on every snapshot. They differ
  exactly when a reconnect landed on a server rebuilt since the page loaded,
  which is the case where the script itself is out of date.
- **A mismatched page does not reload itself.** It could, but a reload during a
  rebuild costs the reader their scroll position and whatever file they had
  open, every time. Showing both versions is enough for them to decide, and
  refreshing is one keystroke they are already used to.

- **The root path is wrapped in `\u202A`/`\u202C`.** Its box is `direction: rtl`
  so a long path is clipped at the front and you keep the useful tail — but that
  also reorders the path's own characters, and the leading `/` (bidi-neutral)
  jumped to the far end, so `/root/www/oss/treex` displayed as
  `root/www/oss/treex/`. The embedding makes it one left-to-right run inside the
  right-to-left box: correct order, ellipsis still at the front.
- **The starting font size lives in the stylesheet's `--fs`, once.** The script
  reads it with `getComputedStyle` rather than repeating the number, so the page

- **The TUI has no preview pane, by choice.** It shows the browser's open file in
  the status bar instead. `treex::preview` stays view-agnostic if that changes.
- **The gutter builds one number per line as a single string**, so a 30k-line
  file is two DOM nodes rather than 30k. Fine up to the 1 MiB preview limit.
- **No auth on `--web` at all, by design so far** — the README says to tunnel it.
  Revisit before anyone treats it as a service.
- **The right mouse button is deliberately inert**, waiting for whatever it is
  going to mean.
- **Filesystem work does not run on a runtime worker.** `Command::may_block()`
  says which commands read the disk, and both the web socket loop and the TUI
  event loop send those through `spawn_blocking`; cursor moves stay inline, since
  a thread hop per arrow key would be silly. Measured with
  `TOKIO_WORKER_THREADS=1` while expanding a 120k-entry directory, an unrelated
  `/favicon.svg` went from **2311 ms to 1 ms**. The realistic trigger was never a
  huge directory anyway — it is a slow or hung network mount, which is what treex
  gets pointed at.
- **`assets/samples/` holds one file of each interesting kind** — text, awkward
  filenames, a lossy-UTF-8 file, an empty file, PDF/ZIP/PNG/JPEG, a 2000-line
  log, a file over the preview limit. Excluded from the published crate. Expand
  it in the browser after touching anything about previews or content types; a
  sweep over it is what turned up the `#`/`?` URL bug and the PDF-as-text bug.

## ## Binary size — measured 2026-08-25, x86-64 Linux, release

Release profile carries `opt-level="z"`, `lto="fat"`, `codegen-units=1`,
`panic="abort"`, `strip`. That alone was worth 4.50 MB -> 2.26 MB with no
dependency changes.

| features | binary |
|---|---|
| `--no-default-features` (library only) | 0.70 MB |
| **default — what `cargo install treex` gets** | **1.58 MB** |

Dropping `ignore` took the dependency tree from 140 crates to 131 and removed
`regex-automata`, `regex-syntax`, `aho-corasick`, `globset` and `bstr`
outright — about 756 KB when it was last measured on its own, more than the
entire HTTP stack costs.

`panic="abort"` is safe here: the TUI restores the terminal from a panic hook,
and hooks still run before the abort. `Terminals::drop` does not, which is why
that hook exists.

## ## Binary size — measured 2026-08-24, x86-64 Linux, `--all-features`

Kept for the profile sweep, which is why `Cargo.toml` carries the settings it
does. The `.text` breakdown below is out of date: the regex stack went with
`ignore`, so read the 2026-08-25 numbers above for what ships today.

Release profile sweep, no dependency changes at all:

| profile | binary |
|---|---|
| `lto="thin"`, strip (original) | 4.50 MB |
| + `codegen-units=1` | 3.83 MB |
| + `lto="fat"` | 3.43 MB |
| + `panic="abort"` | 3.04 MB |
| + `opt-level="z"` **(now in Cargo.toml)** | **2.26 MB** |
| `opt-level="s"` instead of `z` | 2.39 MB |

Where the remaining `.text` goes (`cargo bloat --crates`, its own numbers are
approximate):

| | | |
|---|---|---|
| std | 840 KB | |
| **regex stack** | **~767 KB** | `regex_automata` 320 + `aho_corasick` 209 + `regex_syntax` 177 + `globset` 62, all pulled by `ignore` for gitignore globs |
| `clap_builder` | 280 KB | |
| treex itself | 242 KB | |
| tokio | 151 KB | |
| **HTTP stack** | **~304 KB** | `hyper` 120 + `axum` 107 + `http` 48 + `tungstenite` 29 |
| notify + debouncer | 101 KB | |
| ratatui + crossterm + cassowary | 143 KB | |

The headline: **gitignore support costs more than the entire web server.** The
earlier "axum is +1.10 MB" figure was a whole-binary delta including generic
instantiation; by `.text` attribution the HTTP stack is about 300 KB. Both are
true, they measure different things — do not quote one as if it refuted the
other.
