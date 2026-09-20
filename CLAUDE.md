# treex

One crate, features `tui` / `watch` / `web`. Not a workspace.

- **`docs/DECISIONS.md` is the design rationale.** Read the relevant entries
  before changing the wire protocol, the tree model, the watcher, or the page's
  rendering. Several record something built two or three times already.
- Layout, checks and release policy: `CONTRIBUTING.md`. CLI and features:
  `README.md`. Library API: rustdoc in `src/lib.rs`, as runnable doctests.

Things that will bite before you reach the rationale for them:

- **Every structural mutation goes through `Tree::reshaped()`.** Bumping
  `revision` alone on something that changes the rows leaves every browser
  showing a stale tree.
- **`render.rs` and `hit.rs` share `INDENT`/`TWISTIE_WIDTH`.** Changing one
  without the other makes mouse clicks land on the wrong row.
- **`bat`'s output has to be the same bytes as the file.** `highlight::parse`
  drops the colors entirely when it is not, so the flags in `run_bat` —
  `--tabs=0` above all — are load-bearing, not tidiness. Anything that
  reformats what `bat` prints turns coloring off silently.
- **ratatui patches cell styles, it does not replace them.** Anything drawn
  over something already painted — the file header sits on the pane's top
  border — keeps the colors it does not set for itself. `Style::reset().patch(…)`
  is how the header's name gets the tree's plain file color instead of the
  border's grey.
- **Three decoders share `packed`**: the `Serialize` impl in `tree.rs`, the
  page's `decode`, and the mirror in `tests/sync.rs`. It is
  `kind << 3 | ignored << 2 | symlink << 1 | expanded`; a wrong shift compiles
  fine and silently makes every row the wrong kind.
- **`Tree::select` shows files.** Moving the cursor is not a neutral act any
  more: it changes what both views display. A directory selects without
  displaying, which is what leaves the last file up.
- **A view records the scroll line it landed on, not the one it was told.**
  Both panes clamp a shared line to their own height, and reporting the clamped
  landing back as a scroll makes the two views drag each other. See the
  `view_line` entry in `docs/DECISIONS.md` before touching the sync.
- **Coloring is a server-side job for both views.** The TUI and the browser draw
  the same runs; there is no highlighter in `index.html`.
- **Three places have to agree on the heartbeat**: `HEARTBEAT` in `web/mod.rs`,
  `SILENCE` in `index.html`, and the wait in `page-test.mjs`.
- **Every path in the repository must be checkable-out on Windows.**
  `tool/check-filenames.mjs` enforces it in `check` and `make lint`.
- Views address nodes by path, never `NodeId` — ids are recycled.
