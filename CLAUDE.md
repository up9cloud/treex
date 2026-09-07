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
- **Three places have to agree on the heartbeat**: `HEARTBEAT` in `web/mod.rs`,
  `SILENCE` in `index.html`, and the wait in `page-test.mjs`.
- **Every path in the repository must be checkable-out on Windows.**
  `tool/check-filenames.mjs` enforces it in `check` and `make lint`.
- Views address nodes by path, never `NodeId` — ids are recycled.
