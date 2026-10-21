# osu! Trainer

18-LAB's native practice-copy tool for vosu. Open it from **Dashboard → Tools → osu! Trainer** to receive a write session. Opening `/tools/trainer/` directly is read-only. The selected client/map comes from the authenticated SDK context; all four rulesets are supported.

## Practice copies

Choose 0.5–2.0× or a target BPM, select pitch-preserving stretch or pitch-changing resampling, and optionally override HP/CS/AR/OD. Unlocked AR/OD follow rate-scaled approach/hit windows; locks keep the source value, and an explicit slider override takes precedence. Saved stats are limited to 0–10; rate-derived values beyond that range are visibly capped. AR uses osu!'s 1800/1200/450 ms piecewise curve, OD uses standard 80−6×OD, taiko 50−3×OD, mania 64−3×OD. Changing rate scales the map timestamps, not the live client's playback setting. Base BPM is the **first positive beat length**, so target BPM scales every tempo in a variable-BPM map by the same factor.

- **Stable:** write a new `.osu` and OGG into the selected map's own Songs folder. Never change the original `.osu` or audio. Press **F5** in osu! stable to refresh the song list.
- **Lazer:** never write to its hashed file store. Create an importable `.osz` in `VOSU_TOOL_DATA_DIR/exports/`, then open it using the OS's `.osz` association. If opening fails, the completed job reports the export path for manual import. The OS association must point to lazer. Import cannot be automatically undone; deleting an export does not remove an already imported lazer map.
- Lazer context supplies the source map, audio and background but not its filename→hash asset manifest. Include resolvable referenced assets; omit unavailable/unsafe custom hitsounds, storyboard images or videos with a **visible per-file warning**. The practice map and processed audio remain playable; unavailable sounds fall back to skin/default samples and unavailable visuals are absent. No unrelated hashed-store contents are archived. Resolvable external `.osb` files are retimed in the archive. Stable external `.osb` files remain originals shared by the whole set; in-map storyboard commands are retimed.
- The generated manager uses `index.json` in the tool data directory. Deletion accepts only an indexed ID, confines every file to its recorded map/export folder, rejects symlinks and modified contents, and preserves audio referenced by another generated map. Only generated copies and owned export/audio files are deleted. It never removes the source map/audio or the folder itself. Renamed/moved files are not discovered or deleted.
- Identical requests reuse existing indexed outputs. Audio names derive from source content, effective rate and processing mode, so different stat variants reuse the same processed OGG. Lazer retains that owned OGG beside its export for reuse.

The transformer edits `.osu` text line-wise, retaining unknown sections, comments, slider geometry/sample parameters and set IDs. `rosu-map 0.2.1` cannot round-trip storyboard commands ([upstream API README](../vosu-pp/rosu-map-api/README.md#storyboard)); its Events type only retains background/breaks. Golden fixtures cover timing points (positive beat lengths only), objects including slider/spinner/hold ends, preview/bookmarks, breaks, video/sample offsets, nested loop/trigger commands and animation frame duration. Integer timestamp fields are rounded; timing-point doubles retain precision. New maps have `BeatmapID:0`.

## Audio and jobs

No external media runtime is required. MP3 uses **minimp3** (MIT Rust bindings, CC0 C core), WAV uses **hound** (Apache-2.0), Vorbis decode/encode uses **vorbis_rs** / bundled Ogg and aoTuV/Lancer Vorbis (BSD-3-Clause), high-quality pitch-preserving stretch uses **Signalsmith Stretch** (MIT C++, `cc`-built bindings), and pitch-changing band-limited sinc resampling uses **rubato** (MIT). Output is **OGG Vorbis**, supported by osu! stable and lazer. [Exact licenses](web/licenses.txt) ship with the tool. No SoundTouch/LAME code is linked; no MPL dependency is used. Symphonia was excluded under this release's stricter permissive-only license policy.

Processing is an offline job with progress and cancellation. Decoded audio is held in memory. Cancellation is checked through decode/resampling/encode and before writing the practice map; Signalsmith's latency-compensated `exact` call is indivisible, so cancellation during that call is applied immediately after it returns rather than interrupting C++. Generation and deletion are serialized to preserve the ownership index.

API: `GET /v1/current?rate=1.3` (includes available dependency MSD/dan), `GET /v1/generated`, `POST /v1/generate` (options JSON → `202 {"job":"…"}`), `GET /v1/jobs/<id>`, `DELETE /v1/jobs/<id>` (cancel), `POST /v1/delete/<indexed-id>`. Job/error shapes follow the tool protocol. Mania difficulty labels are experimental estimates supplied by the declared `difficulty` provider, not official rankings.

## Development and checks

From the workspace: `cargo build -p vosu-tool-trainer --locked`, then `vosu --tool crates/vosu-tool-trainer`. C/C++ compiler and libclang are build-only requirements (Signalsmith bindgen); audio libraries build statically from their Cargo packages.

```sh
cargo fmt --package vosu-tool-trainer
cargo clippy -p vosu-tool-trainer --all-targets --locked -- -D warnings
cargo test -p vosu-tool-trainer --locked
# clang-cl cross-check only; the hyphenated cc variable takes precedence over
# cargo-xwin's generated underscore variable. Keep Linux headers out of Windows.
env -u C_INCLUDE_PATH \
  'CFLAGS_x86_64-pc-windows-msvc=-Wno-error=incompatible-pointer-types' \
  cargo xwin clippy -p vosu-tool-trainer --all-targets --locked \
  --target x86_64-pc-windows-msvc -- -D warnings
```

The cross-check flag downgrades a known upstream C pointer conversion diagnostic, not Rust warnings: aoTuV/Lancer 0.1.6 passes float pointers to clang's `const char*` `_mm_prefetch` intrinsic in [`mdct.c` (983–987)](https://docs.rs/crate/aotuv_lancer_vorbis_sys/0.1.6/source/vorbis_vendor/lib/mdct.c#983) and [`window.c` (2177–2207)](https://docs.rs/crate/aotuv_lancer_vorbis_sys/0.1.6/source/vorbis_vendor/lib/window.c#2177). Native MSVC does not need this clang-specific flag. The unqualified cross command fails on recent clang-cl; no upstream source is patched or vendored here.

The `tests/service.rs` smoke runs the actual service against an isolated synthetic Songs folder and mocked authenticated context, generates 1.3× stretch and 0.8× resample, verifies durations and original bytes, prints `.osu` excerpts, checks token rejection and stdin-close shutdown. Set `TRAINER_SMOKE_DIR` to retain generated evidence. `python crates/vosu-tool-trainer/tests/render_ui.py --output /tmp/trainer-ui` renders the actual UI in isolated **headless Firefox** profiles with a mocked bridge/host, saving dark, light and read-only screenshots. It never opens a desktop window or touches the user's running client/daemon.
