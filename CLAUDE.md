# CLAUDE.md — GlassineOS/installer

The guided installer for GlassineOS (see `README.md`): one flow for every machine, device
knowledge in device descriptions, device-specific steps in platform backends. Nothing built
yet; the first spec is `specs/001-install-from-stock/`.

- Inspecting and planning never write to the machine. A change that adds a write needs a
  test of its failure case (cut write, read-back mismatch, an interruption) before it merges.
- Nothing vendor-owned: no firmware, boot loader binaries, partition dumps, keys or serials.
  Vendor files are read from the person's own device and never uploaded or cached.
- Never run the installer against a real device without the owner's go.
- Commits carry an `Assisted-by:` or `Co-Authored-By:` trailer. Never add
  `Signed-off-by`. Run `.glassine/org/scripts/safety-scan.sh --staged` before committing.
- Specs: Spec Kit 1.1.0 with the org preset (`.specify/`, setup in
  `.glassine/org/SPECS.md`); features go in `specs/###-name/`, only `spec.md` is committed.
  The repository principles are the last section of `.specify/memory/constitution.md`.
- Organization guidance for coding agents, including the project knowledge search
  (run `git submodule update --init` if the import below is missing):

@.glassine/org/AGENTS.md
