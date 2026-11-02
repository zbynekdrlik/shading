# shading

Shading (bkshading): remote shading control of the Blackmagic cameras. A relay on each cambox or
handheld SBC drives its camera over USB-PTP; the service on the strih aggregates them into one web
panel. Split from `zbynekdrlik/camera-box` (shading#1).

- `bkshading/proto`, `bkshading/relay`, `bkshading/service`: the crates (one workspace version).
- Releases: GitHub Release `vX.Y.Z` from `main`, assets + `.sha256`. camera-box pins and deploys them.
- `docs/CONTRACT.md`: what camera-box reads. `docs/HANDOVER.md`: project state.
