# VIcky: a research fork of VIctory

VIcky is a **research fork of [VIctory](https://github.com/Marvisak/VIctory)**. Its goal is to find out why the retail title's world renders black or flat grey on PC, and to fix it, in the open. It is not an official release. It is not affiliated with VIctory's maintainers, with XCL, or with the title's publisher.

> **FR —** VIcky est un fork de recherche de VIctory. Le jeu se lance, charge le monde et répond aux commandes, mais la scène 3D s'affiche noire ou grise. Ce dépôt rassemble les meilleures branches de la communauté et des outils de diagnostic, pour trouver ensemble quelle passe de rendu casse et pourquoi. Toute aide est la bienvenue (issues, mesures, correctifs).

**Rule #1, as in VIctory: nothing from the game is ever committed.** That covers captures, shader dumps, extracted tables and raw logs. You supply your own copy of the title at run time. See [docs/asset-policy.md](docs/asset-policy.md); `cargo xtask policy` checks it.

## What is in this tree

| Part | Source |
|---|---|
| Base: VIctory plus about 176 open upstream PRs (wave64 stack #439, #493, #483, #206, #566, #453…) | r4tedx [`integration/testers`](https://github.com/r4tedx/VIctory/tree/integration/testers) at `92bc27b` (see [INTEGRATION.md](INTEGRATION.md)) |
| Mirror 256-byte-aligned CBV tables in place, so the dynamic descriptor slots are not used up (VIctory #840) | alessiodeia `1d21e06` |
| Logs which texture each present plane comes from, for listed frames | fuzeofficlal `9a10d7a` |
| Capture frames by run time (`VICTORY_CAPTURE_AT_SECS`), and trace every draw and dispatch of a captured frame with its programs and the reason it was dropped | this fork |

## Build and run

Same as VIctory: Rust as pinned in `rust-toolchain.toml`, and a `victory.toml` pointing at your copy of the title (see `victory.example.toml`). Use a diagnostics build:

```
cargo build --profile diag -p victory --features diagnostics
target\diag\victory.exe run "<path to the title's exe>" --execute
```

With `VICTORY_SHADER_LOOP_LIMIT=32`, testers report no device removal on several NVIDIA cards.

## Diagnostics this fork adds

- **`VICTORY_CAPTURE=<folder>`, `VICTORY_DUMP_TARGETS=1`, `VICTORY_CAPTURE_AT_SECS=170`.** These save the first frame built after 170 s of the run and every render target it writes, in pass order. Menus take a different number of frames each run, so a time is easier to aim with than a frame number. The world starts about 70 s in.
- **`VICTORY_LOG=victory_gpu::draw=debug,info`.** For that frame this logs every draw and dispatch, in order:
  - `draw … [vertex <hash> + pixel <hash>] targets …`
  - `dispatch XxYxZ compute <hash>`
  
  A program that has no host pipeline is marked `MISSING <reason>`; one that was left out is marked `SKIPPED`.

## What we know so far

Measured on an RTX 4080 (Windows 11), in diagnostics builds of this tree, unless a line says otherwise.

- **The world loads and runs at about 1 frame per second.** About 930–980 draws and dispatches per frame are dropped, of about 2,150; about 250 of those are only waiting for their pipelines to compile. The UI, subtitles and the loading art all render.
- **The G-buffer is partly drawn.** In a world frame (a cutscene), the 2560x1440 G-buffer targets hold some geometry: curtains, a roof, plates, a light shaft. It is striped and noisy, and much of the scene is missing. The depth target matches.
- **Every HDR / lighting target is black.** The post-process output is a flat colour, which is why the screen is grey. So the first link that breaks is lighting, right after the G-buffer.
- **The traced world frame (frame 2514, about 4,500 draws and dispatches) shows where the breaks are:**
  - **Instance culling.** Compute `e30970f1727063fd` is `MISSING` (a structured buffer of unknown stride) **321 times per frame**. It sits between the G-buffer's mesh draws, alternating with computes `1ea7e2dbf731c8dc` and `4115dd63f99a09d7`: the GPU-driven culling that picks what the next mesh draws draw. Without it most objects are never drawn. VIctory's INTEGRATION.md names the same unknown-stride stop for the four instance-culling programs of VIctory #71.
  - **Lighting.** The lighting does run, and it writes the HDR target (`2560x1440 f10`):
    - a full-screen draw: vertex `8157a974717876a9` + pixel `3ded230430ee2590`, also writing a `2560x1440 f61`;
    - then mesh draws: `a26422c01dcf5612` + `6a3a8d3fe52b4eab`, and `f84fa6626772ebf0` + `f9a4d2f8d13f46ef`.
    
    The pixel shader reads a structured buffer of 2,044 elements, a 160x90 `f24` texture and two 3D textures. Its output is black, so its inputs are probably empty. Candidates are the computes still `MISSING` in the frame: `3b90165a77d95cdf` 186× (an address in an SGPR that is no root parameter), `f6e47efa50226beb` 129×, `5552dac6a2b7a1fd` 55×, and about 10 more at around 50× each.
- **The step from the 1440p scene to the 4K output** is the same open question as VIctory #841, where the scene is fully drawn at 1440p and the 3840x2160 output stays zero.
- **Shader translation is still the main source of drops.** The classes that drop the most draws in our runs:
  - `v_readfirstlane` / cross-lane operations in divergent control flow;
  - an address in an SGPR that is no root parameter;
  - a structured buffer of unknown stride;
  - a lane mask read as data.
- **Descriptor heap.** The title's view heap has 1,245,184 slots; NVIDIA caps a shader-visible heap at 1,000,000. The title never reaches its upper slots in our runs.
- **Memory-copy cost (about 2/3 of a frame).** Experiments, not in this tree yet:
  - A pagefile section created with `SEC_COMMIT` and smaller than 4 GiB opens as a host D3D12 heap (`OpenExistingHeapFromFileMapping`). The GPU then reads and writes the title's pages directly, with no copy.
  - At 4 GiB or more, or with `SEC_RESERVE`, opening it fails.
  - Reserved (tiled) resources mapped onto such a heap removed the device.
  - Buffers that are one run of a section can be used in place. The streamed pages are scattered, so the gain so far is small.

## How to help

- Run the tree with the diagnostics above and post what you find in an issue: which pass first writes black, which program is `MISSING` there, your GPU and driver. **Post text only, no game images.**
- Pick a translation class above and look into VIctory's translator (`crates/victory-shader`).
- Related threads: VIctory #841, #850, #840, #71; XCL ivall/xcl#505, #1031.

## Credits and licence

VIctory and every PR in `integration/testers` belong to their authors (see `INTEGRATION.md` and the git history). This fork is GPL-3.0, like VIctory ([LICENSE](LICENSE)).
