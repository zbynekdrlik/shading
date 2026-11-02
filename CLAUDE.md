# shading

Shading (bkshading). Start with `docs/HANDOVER.md`.

## Playbook router

- crates, CI, release, contract → `.claude/rules/bkshading.md` (auto-loads on `bkshading/**`, `Cargo.toml`, CI, `scripts/release.py`, `docs/CONTRACT.md`)
- relay / handheld SBC → `.claude/rules/bkshading-sbc.md` (auto-loads on `bkshading/relay/**`)
- `docs/CONTRACT.md` is pinned by camera-box: a change there is a breaking change (bump, tell camera-box).
