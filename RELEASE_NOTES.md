# bb 0.17.5: every file `bb` writes is a v2 file with a version commitment; interrupted uploads restart

This release moves `bb` to Beebeeb core v0.3.0. Core 0.3 refuses to open a v2 version whose metadata carries no version commitment, so every writer in `bb` now seals one.

### What changed

- **All new files are v2.** `bb push`, `bb sync`, `bb backup` and `bb webdav` write core's current format, and `bb repair` rewrites a repaired file as v2.
- **Each version carries its commitment**: frame 0's id and a counter that rises by one per replace. It goes out with the upload's `init` and with `complete`, as the desktop and web apps do. A replace (`bb push --replace`, a `bb sync` edit, a WebDAV PUT over an existing file) continues the counter of the version it replaces.
- **Renames and moves keep the commitment**: `bb mv`, `bb webdav` MOVE and the moves `bb backup` makes re-seal the name with the commitment it already had, so the file stays readable.
- **`bb pull --zip` passes each entry's commitment to core.**

### Resuming interrupted uploads is gone

Core 0.3 never continues a v2 upload session: the version id is bound to the frames already stored. A push that was killed (Ctrl-C, a lost connection, a reboot) therefore starts that file again from the beginning, however far it had got. Files that finished are not sent again.

### Known limits

- Creating a file works against any server. Replacing a v2 file (`bb push --replace`, a `bb sync` edit, a WebDAV PUT) and `bb repair` send the version metadata at `init` (the `version-metadata-v1` capability, which `bb` declares) and need a server that takes it; the server's answer is shown if it does not.
