# Legacy PowerPoint files

> **Last reviewed:** 2026-11-09 · **Last updated:** 2026-11-09 · **Change:** minor (initial import/export support; compatibility remains partial)

Open `.ppt`, `.pot` or `.pps` through File › Open, drag and drop, the command line, or the
control channel. Detection uses the file contents, so a binary `.ppt` renamed to `.pptx` also
opens. A `.pps` opened by the system starts a slide show. Encrypted presentations are rejected
with a password-protection message.

Save through File › Save As › **PowerPoint 97-2003 (*.ppt)**. The engine also accepts
`file.saveAs`, `file.export` and `file.saveBytes` with `format: "ppt"`. These produce a genuine
compound binary PPT file. Saving does not require PowerPoint or an external converter.
Imported legacy files start without an overwrite path: choose a new name to preserve the source.

The reader reuses [PR #1137](https://github.com/storytold/deckcraft/pull/1137). It supports slides,
masters, formatted text, shapes, pictures, speaker notes and hidden slides. It does not import
animations, transitions, links, actions, audio/video or PICT pictures. Tables become grouped
shapes, charts and OLE objects use their previews, and WordArt becomes plain text.

The writer keeps supported basic shapes, PNG/JPEG pictures and text editable, including Unicode,
basic run formatting, paragraph alignment, bullets, spacing and margins. It retains speaker notes
and hidden slides. A slide requiring unsupported drawing features is rendered as one PNG picture;
its appearance is retained but its objects are no longer individually editable in the PPT file.
Animations, transitions, hyperlinks and embedded media are omitted. Compatibility messages are
shown on import/save, and automation responses expose the warnings. Keep a `.deckcraft` or
`.pptx` copy for full editing.

Export is bounded to 1000 slides, slide sides from 1 to 4032 points, 256 MiB of presentation
streams and 128 MiB of generated previews. A preview's longest side is at most 2048 pixels.
The writer fails explicitly when these limits are exceeded.

The implementation follows the public [MS-PPT](https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-ppt/),
[MS-ODRAW](https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-odraw/) and
[MS-CFB](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-cfb/) specifications.
Tests generate their own records and files; no PowerPoint-produced fixture is committed.

Validation includes synthetic import/export and malformed-input tests, the Save As UI test,
and Apache POI 5.4.1 independently reading exported Unicode text, formatting, shape bounds,
notes and PNG preview references. Actual PowerPoint interoperability and broad real-world
corpus fidelity remain unverified.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-09 | minor | Initial bounded import/export, compatibility messages, UI integration and synthetic/interoperability checks. |
