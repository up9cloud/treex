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
- **`viewing` lives in the shared session, not in the web module.** Written when
  only the browser could show a file and the terminal merely marked the row —
  even then the state had to travel the same path as the selection. It is what
  made the terminal's own file pane a view of state that was already there
  rather than a second copy of it. A reconcile that drops the file clears it, so
  neither view is left pointing at something gone.
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
- **What git ignores is dimmed, never hidden — and git is what decides.**
  The old entry here said there was no `.gitignore` support on purpose, and the
  reasoning still stands for what it was about: *hiding* files by project
  convention is a different job from browsing a directory. Dimming is not
  hiding. Nothing disappears, nothing needs a flag to get it back, and
  `target/` is still there to open when that is what you came for.
  What changed is the cost. The old support meant the `ignore` crate — the
  single most expensive thing in the binary, 756 KB of regex stack. This asks
  `git check-ignore` instead: no dependency, and it is not an approximation.
  Ignore rules are not a glob list. They nest, they negate, later rules
  override earlier ones, and the answer depends on `core.excludesFile`,
  `.git/info/exclude` and whether the path is already tracked. A test in
  `git.rs` pins the one that catches everybody: `!spared.log` followed by
  `*.log` does **not** spare the file, because the later rule wins.
  Three things fall out of asking git rather than parsing files:
  - **One call answers a whole directory**, run with that directory as its
    working directory. git discovers the repository itself, so a folder holding
    a dozen unrelated checkouts needs no bookkeeping and a directory in no
    repository simply reports nothing. Measured: ~1–3.5 ms per directory, paid
    only when that directory is expanded.
  - **A tracked file is never dimmed**, even when it matches a pattern,
    because `check-ignore` consults the index. That is the right answer for
    something on screen, and it is free.
  - **Nothing below an ignored directory is asked about.** git cannot
    re-include a path whose parent is excluded, so the whole subtree is settled
    by one answer.
  **What it costs, measured 2026-09-20.** One `check-ignore` is about 1.1 ms,
  and almost all of that is starting a process: 1 path takes 1 ms, 5,000 take
  3 ms, 20,000 take 7 ms. So the price is per directory, not per entry.

  | | with git | without |
  |---|---|---|
  | this repo, everything expanded (48,539 rows) | 218 ms | 148 ms |
  | `r` on the same tree | 172 ms | 151 ms |
  | a repo of 801 directories, none ignored | **881 ms** | 4 ms |
  | `r` on that | **984 ms** | 4 ms |

  This repo is cheap because `target/` and `.git/` are ignored and the
  short-circuit skips their subtrees, which is most of those 48,000 rows — the
  optimisation earns its keep on real checkouts. A tree whose directories are
  all tracked pays 1 ms each, which is invisible expanding one at a time and
  about a second for `E` or `r` over eight hundred of them. Both of those go
  through `spawn_blocking`, so it is a wait rather than a freeze.
  If that ever bites, the fix is a **long-lived `check-ignore --stdin`** per
  repository: the measurements say the fork is the cost, not the question.
  Batching across directories instead would fight the lazy model and break on a
  folder holding several repositories, which is the case this design gets right
  for free. Left as it is until someone feels it.
  Only the entries that survive `max_entries` are asked about; sending git the
  95,000 names a huge directory does not keep would be work for an answer
  nobody reads.
  `.git` itself is the one deviation: git does not call it ignored — it is the
  repository, not something untracked — but it is not what anyone opened a tree
  to read, so treex dims it anyway.
  A changed `.gitignore` rewrites the answer for everything below it while
  changing no listing at all, so the ordinary refresh would see nothing to do:
  the watcher sends `RefreshSubtree` for it instead. Reads are excluded from
  that — opening a `.gitignore` in the viewer is an event too, and re-reading a
  subtree for it once cost a full snapshot to every browser before a test
  caught it.
- **The dim bit rides in `packed`, which moved `kind` up a bit.** `packed` is
  now `kind << 3 | ignored << 2 | symlink << 1 | expanded`. Three decoders have
  to agree: the `Serialize` impl in `tree.rs`, the page's `decode`, and the
  mirror of it in `tests/sync.rs`. Getting the shift wrong does not fail to
  compile — every row silently becomes the wrong kind — which is exactly the
  failure `page-test.mjs` exists for, and why the test that covers this asserts
  the kinds as well as the dimming.
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
- **The cursor shows whatever it can; a directory leaves the last file up.**
  This is the fourth shape this has had, and the first that matches what an
  editor does. `Tree::select` displays the node it lands on, so one click or one
  arrow key is the whole gesture — there is no "merely highlighted" state left
  to explain. A directory has nothing of its own to show, so what is on screen
  stays: moving through a tree is not a reason to blank the pane being read.
  That leaves exactly one way for the cursor and the pane to disagree — the
  cursor parked on a directory — and `select_viewed` closes it: working the
  pane (scrolling it, clicking its name, clicking into it) says the file is the
  subject again, so the cursor rejoins it.
  What this replaced: `viewing` as a second step on top of `selected`, with
  `→`/`Enter`/double-click to enter it and any cursor movement to leave. It was
  built three times and defended twice in this file; the thing that finally
  settled it was asking what VS Code does, which is this.
  The cost is real and was accepted knowingly: arrowing down a directory reads
  every file it passes, and in the terminal that is a `bat` process each. Both
  views drop a read whose file is no longer the one on screen, so the work is
  wasted rather than wrong.
- **One press opens a file, so `Clicks` is gone.** Pairing presses into double
  clicks was a hundred lines and four tests in service of a distinction that no
  longer exists. A second press on a row is the same press.
- **`←` never closes a file any more.** It used to close first and collapse
  second, which only worked while having a file open was unusual. Something is
  nearly always up now, so in the tree `←` collapses or goes to the parent, and
  in the file pane it hands the keyboard back to the tree — the file stays
  where it is, because there is nothing to leave. The footer says which it will
  do: `← up` with the keyboard in the tree, `← tree` with it in the file.
  Closing outright is left to the browser, where `←` is how a phone gets its
  tree back; in the terminal both panes are always there once a file has been
  touched.
- **The cursor is brought into view when it moves, not on every frame.** The
  wheel over the tree did nothing at all before that distinction existed: it
  scrolled, and the next draw pulled the offset straight back to keep the
  cursor on screen. Now `clamp_offset` is only given a cursor to reveal when
  `selected` actually changed, so the wheel scrolls away from the cursor and
  stays there until a key moves it — which is what every file explorer does,
  and it is also why the wheel does not move the cursor itself: rolling past
  fifty files would read fifty files.
- **The cursor's color says where the keyboard is.** Grey while the tree has
  it, purple once the file does — because `↑↓` then means two different things
  and there is nothing else on screen that says which. The purple used to mean
  "this row is a file being read", which stopped being information the moment
  the cursor displayed whatever it landed on: every file row would have worn
  it. The displayed file is no longer marked separately either; it is the
  cursor row almost always, and the file pane's own header names it the rest of
  the time.
- **The keyboard follows a deliberate open, not the cursor.** `Enter` and `→`
  mean "open this", so they hand the file pane the keyboard. The cursor merely
  landing on a file shows it without taking the keyboard away — otherwise `↓`
  would move the cursor once and then start scrolling the file it had just
  arrived at.
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

- **The TUI has a file pane.** It used to show the browser's open file in the
  status bar and nothing more, on the theory that a terminal tree does not want
  a reader in it. That stopped being true the moment the files were worth
  looking at in color — the terminal is where the tree is actually browsed.
  Nothing had to be added to the session to do it: `viewing` was already shared
  state and `treex::preview` was already view-agnostic, exactly as the old entry
  said it would have to be. The pane reads the file itself rather than being
  handed one, so the terminal and the browser each hold their own copy and
  neither waits on the other.
- **The file pane has its own focus, and `Tab` moves between them.** `↑↓`,
  `PgUp`/`PgDn` and `g`/`G` go to whichever pane has the keyboard; the keys that
  are not about moving through a file — `.`, `r`, `z`, `q` — always reach the
  tree. A press or the wheel acts on the pane under the pointer, which is the
  one place position decides rather than focus.
- **Whitespace is drawn by default.** `·` for a space, `→` for a tab, `␊` for
  a line ending and `␍` before it where there is one — `bat`'s vocabulary,
  since that is what the rest of this pane already speaks. Trailing whitespace
  and a stray carriage return are exactly what a file viewer should not hide,
  and nothing moves when they are turned on: the tab's arrow takes the first
  cell of the stop that was already being padded.
- **Copying is the terminal's job; treex only gets out of the way.** Four
  toggles — `b` folds the tree (and with it the file pane's border, which has
  nothing left to separate), `#` hides the line numbers, `,` hides the
  whitespace marks (a `·` on screen is a `·` in the clipboard), `m` releases
  the mouse — and column one is the first character of the file. Folding also has a
  `«` / `»` control on the file's header, clickable in both views and in the
  same place in both: the terminal writes it over the top border where a title
  would go, and draws it as the pane's own first line once there is no border
  left to write on. `viewer::fold_zone` is the one place that decides where it
  is, so the marker and the click target cannot drift apart — the same reason
  `render.rs` and `hit.rs` share their indents. All four are
  needed: an ordinary terminal selection flows from one line into the next, so
  a gutter left on screen is dragged in with the code, a `·` drawn for a space
  is a `·` in the clipboard, and a captured mouse never reaches the terminal at
  all.
  `c` sets all four at once and puts them back on a second press, because
  wanting to copy something is one thought rather than four. It remembers what
  it displaced rather than toggling blindly, so a reader who had already folded
  the tree does not get it back on the way out.
  The header carries the file's size and line count as well as its name, since
  the file is always on display and its measurements belong with it rather than
  in a footer shared with the tree; the tree's own border carries its row count
  for the same reason. The name wears whatever that file wears in the tree,
  dimmed included, read from the row on every frame rather than remembered when
  the file was opened — so editing a `.gitignore` moves both at once.
  The alternative was tried first and thrown away: a key that dropped out of
  the alternate screen, printed the file to the ordinary one and waited. It
  worked, and it read as a mode switch nobody asked for. What it was really
  buying — selection across the whole file via scrollback, and no clipping of
  long lines — is worth less than not having a second screen to explain.
  Not OSC 52 either, whatever its appeal: macOS Terminal has never supported
  it, any layer in between can swallow it silently, and a `pbcopy` fallback
  would run on the machine treex is on, which is the wrong machine when the
  reader is somewhere else.
- **The footer reports state, not actions, and lit means on.** `# Line#` lit
  is numbers showing, `, ·→` lit is whitespace drawn, `b «` lit is the tree
  showing, `m mouse` lit is treex holding the mouse, `c copy` lit is the
  arrangement engaged. An indicator that named the next action instead read as
  a lie about the present one, which is why the status line became a `Line` of
  separately styled spans rather than one dim string.
- **Each indicator is also a button.** The keys are for hands already on the
  keyboard; a pointer should not have to learn them. That means the labels have
  to be measured exactly where they are written — in display columns, not bytes
  and not characters, since the note in front of them can be a CJK path at two
  columns per character. `status_line` returns the zones alongside the line it
  built, from the same walk, so a button cannot end up beside its word rather
  than on it.
- **The footer is ordered by what each key acts on, widest scope first.** `m`
  and `q` are the program itself — the mouse belongs to treex or to the
  terminal, and leaving is always available. Then the arrows, which act on
  wherever the cursor is. Then `.`, which acts on the tree. Then `c`, `#`, `,`
  and `b`, which act on the file. It is not an order by frequency and not one
  by symmetry, which is why it is written down: both are tempting and both
  scatter the grouping.
  Where the line falls is not a judgement call — it is already in the code as
  `has_file`. `b`, `#`, `,` and `c` are guarded by it and vanish from the
  footer without a file; `m`, `q`, the arrows and `.` are not. So whatever a
  key appears to act on, its group is the one its existence depends on. `b`
  folds the *tree* and still belongs with the file, because there is no `b`
  until a file is up.
- **A modifier makes it a different key.** The mode toggles matched the
  character alone, which quietly took `Ctrl-C` away the day `c` became copy
  mode: the chord arrives as `Char('c')` with `CONTROL`, and the bare-`c` arm
  caught it first. Ctrl, Alt and Super now disqualify a toggle; Shift does not,
  because `#` is Shift-3 on most layouts and terminals disagree about whether
  they say so.
- **Below 72 columns the file takes the window.** `TREE_MIN_WIDTH` (26) plus
  `VIEWER_MIN_WIDTH` (46): a tree narrower than the first cannot show a nested
  name, and a file narrower than the second is not worth reading. Two fifths to
  the tree above that, capped at 60 columns.
- **The divider drags, and both of its columns are grabbable.** The two panes
  each draw a full border, so the divider is two characters wide — and a
  one-column target is hard to hit with a mouse and impossible with a finger, so
  both count. Three things it would be wrong to leave out: the press remembers
  how far it landed from the edge, or the panes jump a column before the drag
  even starts; the width is clamped as it is *stored*, not only as it is drawn,
  or a width dragged past the limit has to be dragged back through the slack
  before anything moves; and the dragged width outlives closing the file,
  because a ratio someone chose is not a property of the file they chose it in.
  It does not outlive the process — nothing in treex does, and expansion state
  is the bigger prize there. The 60-column cap applies only to the default: past
  it is a deliberate answer to a question the default was guessing at.
  Verified by sending SGR mouse sequences through tmux, which is the only way to
  see that the press-then-drag arithmetic is right.
- **Syntax coloring runs the user's `bat`; it does not link a highlighter.**
  Measured before choosing, all with treex's own release profile:

  | | |
  |---|---|
  | `bat` as a library | **187 crates** — treex has 131 in total |
  | `syntect`, smallest useful feature set | **+1.88 MB**, 13 new crates, and the regex stack back — the thing dropping `ignore` was worth |
  | `two-face` (bat's own syntax and theme packs) | 1.43 MB of assets on top of that |
  | **running the `bat` that is already there** | **+60 KB, no new dependency** (1.33 MB -> 1.39 MB) |

  Size is not the deciding argument, though: ownership of the theme is. A
  linked engine would have treex's colors, a treex theme flag and a treex
  syntax list to keep current. Shelling out means `BAT_THEME`, `--map-syntax`,
  the user's config file and every syntax their `bat` was built with, for free
  and forever. The cost is a fork and exec per file opened, and nothing at all
  where there is no `bat` — files are then plain text, which is what the TUI
  showed before this existed.
- **`bat`'s output must be the same bytes as the file, or the colors are
  dropped.** `highlight::parse` walks the ANSI against the content treex already
  read and gives up on the first byte that differs, because a coloring that is
  off by a character is worse than none. That check is what makes the flags safe
  to rely on: `--tabs=0` (bat expands tabs by default, and every run after the
  first tab would then be on the wrong characters), `--style=plain`,
  `--wrap=never`, `--paging=never` — all of which a user's config file can
  otherwise turn on. Tabs are expanded when the TUI draws them instead; the
  browser has `tab-size`. `COLORTERM=truecolor` is forced so the browser gets
  RGB rather than a palette index it cannot resolve.
  The `bat` first on a `PATH` may also not be bat at all — Debian gives the name
  to another package, which is why it ships `batcat` — so the version string is
  checked once and the result cached.
- **Coloring is done on the server for both views, and sent as a palette plus
  runs.** The alternative was a highlighter in the page, which would mean two
  implementations disagreeing and a phone parsing source code. The content is
  already on the wire, so the colors do not repeat it: `paint` is a list of
  distinct styles and a list of `[length, style]` pairs covering the file in
  order. Lengths are in **UTF-16 units**, counted in Rust, because that is what
  `String.prototype.slice` measures — counting bytes there would cut every file
  with an emoji in it to pieces. A dozen styles name themselves once instead of
  appearing on every span.
- **Where a file is scrolled to is shared state, like which file is open.**
  `Tree::view_line` sits next to `viewing` and travels the same way, so a phone
  and a terminal look at the same part of the same file. It bumps `revision`
  but not `shape`, which is what keeps it on the 59-byte cursor message: a flick
  of a finger must not cost a snapshot per frame. Each view clamps the line to
  its own height rather than being told what it may show — that is what lets a
  20-row terminal and a tall browser follow each other, with the shorter one
  sitting at its own end.
  Two rules stop the views fighting over it, and both were needed:
  - **Only a scroll that actually moved this view is sent.** A pane already
    showing the last line has nothing to say, so pressing `↓` against the end of
    a short terminal does not drag a taller browser back up to meet it.
  - **A view records where it landed, not the line it was told.** The browser
    clamps a `scrollTop` past the end, and the scroll event that follows is
    indistinguishable from a person's — so the page reported that landing back
    as a scroll of its own and hauled the terminal up. Found in a real browser,
    against a real terminal; `page-test.mjs` only catches it because its fake
    DOM was taught to clamp `scrollTop` the way a browser does.
  Wrapped text is left out of the sync altogether: a wrapped line is several
  screen rows, so no line corresponds to the top of the viewport, and following
  an approximate one would put the other view on the wrong part of the file.
  The page throttles to one message per 100 ms; the terminal sends one per
  keypress, which is already slower than that.
- **The browser is a tree beside a file, not a file over a tree.** The viewer
  used to be an overlay at `inset: 0` — opening a file hid the tree, which is
  what a phone wants and nothing else does. It is now the right-hand pane, with
  the tree as a three-tenths sidebar (min 170px, max 420px), and clicking the
  file's name folds the tree away and back. The phone case is kept by a media
  query rather than by a layout: under 700px the tree is hidden whenever a file
  is up, and `←` brings it back. Two things this costs, both handled: the rows
  only exist while they are on screen, so a tree that was `display: none` comes
  back empty unless something repaints it — and that repaint must happen *only*
  when the window actually changes, or every cursor step onto a file rebuilds
  thousands of nodes and undoes the cursor message. `close()` also has to forget
  what it was showing: closing a file and clicking the same one again arrives as
  a single message, and `syncViewer` would take it for the file already up.
- **The browser paints its own backdrop behind a colored file.** `bat` colors
  for a terminal whose background already matches its theme — it never emits a
  background of its own. A page has its own background, so Monokai in a
  light-mode browser is `#f8f8f2` text on white: invisible. Found by looking at
  a real browser, not by any test. The page picks the backdrop from the palette
  instead: the style covering the most characters is the file's ordinary text,
  and its luminance says whether the theme is a dark one. Light ink gets a dark
  paper and vice versa, the gutter drops the page's bar color for a neutral
  wash, and the nav around it stays the page's own chrome. The terminal needs
  none of this — the user's terminal background is the one their `bat` theme was
  already chosen against.
- **Files over 256 KiB are not colored.** `bat` takes 61 ms on 64 KiB, 203 ms on
  256 KiB and 804 ms on 1 MiB, and a megabyte of Rust is about 240,000 runs for
  the browser to turn into elements. The preview limit is 1 MiB, so a file
  between the two is still read — just in one color.
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

Re-measured 2026-09-11 on a newer toolchain, which is worth more than the table
above for anything but the ratios: **1.33 MB** before the file viewer and
**1.39 MB** with it. Compilers shrink binaries between releases; do not read a
drop since August as something this repo did.

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
