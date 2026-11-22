# AGENTS.md

SSX 3 in the browser: a front end of the game (`ssx3-decomp`, the git
submodule `ssx3/`) that uses `ssx3-engine` as a library. Read `README.md`,
then `ssx3/docs/frontends.md` (the engine API).

## Rules

- **Only the documented engine API.** `crates/web` and `crates/vuworker`
  use what `ssx3/docs/frontends.md` documents (`ssx3-engine` and its
  re-exports; the VU helper `ssx3-render`'s `vujob::run`). When the front end
  needs more, change `ssx3-engine` in `ssx3-decomp` (a pull request there,
  documented in `frontends.md`), then bump the submodule. Never edit files
  under `ssx3/` in this repository.
- **The game data.** `ssx3/` needs `python3 tools/setup.py` (run in
  `ssx3/`) before anything builds: it makes `orig/` and `generated/` (ee,
  iop, vu) from the disc image. Run it again after bumping the
  submodule.
- **One heavy process at a time, machine-wide** (a build, a test run, a game
  run): another session may be building `ssx3-decomp` on the same machine.
  Check `pgrep -fl 'cargo|rustc|ssx3-headless|recomp-ee'` first and wait for
  it. Wrap long commands in `caffeinate -s`. Limit build jobs (`-j 4`, as
  `build.sh` does).
- **Bumping the submodule.** Pin a commit on `ssx3-decomp`'s `main`; run
  `python3 tools/setup.py` in `ssx3/` again, build, and smoke-test the
  browser (below): a race for a minute or more, all guest threads switching,
  no crash. The game's own gate (race replay RAM) runs in `ssx3-decomp`.
- **Versions.** This repository has its own `Cargo.lock`: `wgpu` must
  resolve to the same version as `ssx3-gfx`'s (one `wgpu` in
  `cargo tree -d`), and the `wasm-bindgen` CLI must match the lock's
  `wasm-bindgen`.
- **Commits:** very brief messages. Never add a `Co-Authored-By` line.
- **Pushing:** `main` of this repository, with the submodule pinned to a
  commit on `ssx3-decomp`'s `main`.

## Checking a change

CI (`.github/workflows/ci.yml`) runs, without the game: `npm ci`, then
`npx oxlint --deny-warnings www` and `npx oxfmt --check www` (format with
`npm run fmt`); `rustfmt --check` on the crates; and clippy of the VU helper
for wasm32 against stub generated code (`ssx3/tools/setup.py --stubs-only`).
The front end itself needs the game: build it locally.

`sh build.sh`, then `python3 serve.py` and open `http://localhost:8000` in a
browser with WebGPU, choose the ISO. Check: boot to the menus, a race
(`?SSX3_EVENT=ARA1`), sound, the console's `N VU helper workers` line
(cross-origin isolation works), the page hidden and shown again (pauses and
resumes).

## Bumping the submodule

Move `ssx3/` to a commit on `ssx3-decomp`'s `main`, run `python3 tools/setup.py`
in `ssx3/`, `sh build.sh`, and smoke-test as above. The replay that proves the
game runs exactly is `ssx3-decomp`'s gate (its `AGENTS.md`); the browser runs
the same code. A crash after a bump is most likely a deeper call chain than a
JSPI stack holds, or a new blocking call: look at the console's stack first.

## No game material

Never commit the ISO, `orig/`, anything under `generated/` (in the
submodule it is untracked anyway), the ELF (`SLUS_207.72`), RAM or VRAM dumps
(`*.bin`) or the wasm builds (`www/pkg*`: they contain the compiled game
code). CI checks it (`.github/workflows/no-game-material.yml`).
