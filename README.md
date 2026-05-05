# pdfsigner

A minimal desktop PDF signer for Linux. Drop a PDF, place a signature image
and a date, save. Single binary, no cloud, no account, no proprietary stack.

The UI follows a zathura/feh-style aesthetic: black bars, monospace text,
keyboard- and mouse-driven, no toolbar buttons cluttering the document area.

## Why

I needed a fast way to sign and date PDFs locally without launching a heavy
office suite, without uploading documents to a web service, and without
losing the original PDF (some tools rasterize the whole document on save,
which destroys text searchability and bloats file size).

The available options didn't fit:

- **Xournal++** works well and was the inspiration, but ships a full
  Cairo/Pango/GTK stack and is annotation-focused rather than signing-focused.
- **LibreOffice Draw** opens PDFs but the round-trip is destructive and the
  workflow is heavy.
- **Online signing services** require uploading the document, which is the
  exact opposite of what I want for confidential paperwork.
- **Adobe Acrobat / Master PDF Editor** are commercial.
- **CLI hacks** (qpdf overlays, LaTeX templates) work but require fiddling
  every single time.

`pdfsigner` is the smallest thing that does the actual job: open PDF, drop a
signature image, type the date, drag both into position, save. That's it.

## What it does

- Opens existing PDFs without rasterizing them — original text and structure
  stay intact, the signature/date are added as a true overlay layer
- Inserts signature images (PNG/JPG with transparency) from a configurable
  signatures directory via right-click menu (feh-style)
- Inserts text labels with the bundled Inter font, in three preset colours
  (black, red, blue) or any custom HSV-picked colour
- Inline text editor: press `t` over the page → type directly into the
  spawned label, like Xournal++
- Quick date stamps: `s` for German (`04.05.2026`), `Shift+S` for US
  (`05/04/2026`), `x` for an `X` checkbox mark
- Multi-selection via rubber-band drag (or Ctrl+click), bulk move and
  bulk delete
- Resize selection with `+`/`-` or mouse wheel (text in pt-steps, images
  proportionally)
- Saves the result next to the original as `<name>_signed.pdf`
  (auto-numbered if the file exists)
- Optional page-range filter (`1-3,5,7-9`) at save time to extract only
  selected pages into the signed output
- Mouse-wheel page navigation (when nothing is selected)
- Recovery path for malformed PDFs: if `lopdf` can't parse the input
  (broken xref, compressed object streams, incremental updates), the file
  is round-tripped through `pdfium` to normalize the structure before
  applying the overlay

## What it doesn't do

- **Cryptographic / PAdES digital signatures** — there is no PKI, no
  certificate handling, no `Sig` dictionary. The output is a visible
  signature and a visible date. If you need legally binding qualified
  electronic signatures (QES), use a tool that implements PAdES
  (`pyHanko`, `JSignPdf`, qualified service providers).
- Form filling, OCR, page reordering beyond the page-range filter,
  highlighting/strikethrough annotations, freehand drawing.

## Install

### Runtime dependency: pdfium

`pdfsigner` uses Google's PDFium for page rendering and as a fallback PDF
normalizer. It needs `libpdfium.so` available at runtime.

Arch Linux:

```
yay -S pdfium-binaries
```

Other distros: download a release from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries)
and place `libpdfium.so` either system-wide (`/usr/lib/`) or next to the
`pdfsigner` binary.

### Install from crates.io (once published)

```
cargo install pdfsigner
```

### Build from source

```
git clone https://github.com/rudolfschmidt/pdfsigner
cd pdfsigner
cargo build --release
./target/release/pdfsigner some.pdf
```

A wrapper script for convenience (`~/bin/pdfsigner`):

```bash
#!/usr/bin/env bash
exec ~/dev/github/pdfsigner/target/release/pdfsigner "$@"
```

## Usage

Open a PDF — either as command-line argument or by dragging the file into
the window:

```
pdfsigner contract.pdf
```

### Hotkeys (over the page area)

| Key            | Action                                                         |
|----------------|----------------------------------------------------------------|
| `s`            | Stamp today's date in DE format (`DD.MM.YYYY`) at the cursor   |
| `Shift+S`      | Stamp today's date in US format (`MM/DD/YYYY`) at the cursor   |
| `x`            | Insert literal `x` at the cursor (for checkboxes)              |
| `t`            | Spawn an empty text label at the cursor and enter inline edit  |
| `d`            | Delete the current selection                                   |
| `Del` / `BkSp` | Delete the current selection                                   |
| `c`            | Toggle a colour picker for the selected text overlay(s)        |
| `+` / `-`      | Resize selection — text in pt-steps, images proportionally     |
| `Ctrl+S`       | Save the signed PDF                                            |
| `Ctrl+D`       | Duplicate the current selection (offset slightly)              |

Stamps and `x` are placed with the cursor at the label's bottom-left
corner; `t` puts the cursor in inline-edit mode (Enter for a newline,
Esc or Shift+Enter to finish, empty text removes the label).

### Mouse

- **Left-click an overlay** — select it. On a text overlay this also
  enters inline-edit mode.
- **Ctrl+left-click** — toggle that overlay in the multi-selection.
- **Left-drag from empty space** — rubber-band rectangle to select all
  overlays inside.
- **Left-drag a selected overlay** — move it (multi-selection moves
  together).
- **Right-press over the page** — opens the signature menu (feh-style:
  hold the right button, drag to a menu entry, release). The signature
  is inserted at the original right-press position, centred on the cursor.
- **Mouse wheel** — with a selection, resizes it; without, paginates the
  document.

### Colour picker

Press `c` while a text overlay is selected. The popup has three preset
colours plus a `Custom…` entry that opens a full HSV picker. Live-applies
to all selected text overlays as you drag the picker.

### Header

The header is a thin black bar with monospace 12pt text:

- **Left**: current label size in pt (only while a text overlay is
  selected or being edited), plus a `N selected` indicator for
  multi-selection.
- **Right**: page-range filter input. Type something like `1-3,5` to
  restrict the saved PDF to those pages; leave empty for all pages.

### Footer

Zathura-style modeline:

- **Left**: full path of the current PDF.
- **Centre/right**: status messages (errors in red).
- **Far right**: `[current/total]` page indicator.

### Output naming

Saving `~/Documents/contract.pdf` writes `~/Documents/contract_signed.pdf`.
If that file already exists, the next save uses `contract_signed_1.pdf`,
then `_2.pdf`, etc. The original PDF is never touched.

## Configuration

### Signatures directory

`~/.config/pdfsigner/signatures/` is created on first launch. Drop your
signature PNG or JPG files there and they appear in the right-click menu
the next time you start the app. Only one level deep; no recursion.

For transparency, use a PNG with an alpha channel — the embedder writes a
soft mask so the signature blends with whatever is underneath in the PDF.

### Fonts

Inter Regular is bundled into the binary, no additional installation needed.
Bold/italic are not bundled.

## Privacy notes

This tool does no network traffic and writes nothing outside the output
file location and `~/.config/pdfsigner/`. There are still a few things to
be aware of about what ends up in the signed PDF:

- **Original PDF metadata is preserved.** Title, Author, Subject, Keywords,
  Producer, CreationDate from the source PDF carry over into the signed
  output. If you sign a PDF you received from someone else and forward it,
  the receiver can still read those original metadata fields.
- **Original `/ID` is preserved**, so a signed file can be linked back to
  its source by anyone comparing the two.
- **`pdfsigner` does not add its own `/Producer` fingerprint** to the output.
- **Image EXIF data is stripped** when a signature image is embedded — the
  embedder recompresses the pixels into a fresh PDF image XObject and does
  not propagate any EXIF/ICC metadata from the source PNG/JPG.
- **Image paths are not stored** in the PDF, only the pixel data.
- **When the pdfium fallback is triggered** (broken-xref PDFs), pdfium may
  rewrite metadata fields with its own values, which actually destroys some
  of the original metadata as a side effect.

If preserving privacy from the receiver is important, scrub metadata
separately with `qpdf --linearize` plus `exiftool -all=` before signing,
or open the file in a metadata-aware tool first.

## Architecture

A single Rust binary split into seven small modules (~2k LOC total):

- `main.rs` — App state, lifecycle, event dispatch, panel rendering
- `pdf.rs`  — pdfium loading + lopdf save flow, Inter-font embedding,
  image embedding, page-range parsing
- `menus.rs`— sig menu, preset colour menu, custom HSV picker (all
  rendered as foreground `egui::Area` popups)
- `editor.rs` — inline `TextEdit` with a per-glyph layouter that
  re-colours the selected character range
- `overlay.rs` — `Overlay` enum, geometry helpers (rect, hit-test),
  cursor-range / char→byte helpers, the shared selection-layouter
- `theme.rs` — colour constants and `apply_global` / `apply_header` /
  `apply_popup` style helpers
- `signatures.rs` — signature-image discovery

Underlying crates:

- **eframe / egui** — immediate-mode UI rendered via OpenGL, no system
  GUI toolkit dependency
- **pdfium-render** — PDF rendering for the editor view and structural
  normalization fallback
- **lopdf** — PDF object/stream construction for the overlay layer
- **ttf-parser** — read Inter's TrueType metrics for embedding as a Type0
  CIDFontType2 PDF font
- **image** + **flate2** — image decoding and PDF stream compression
- **chrono** — date formatting for the date stamps

Save flow: load original via `lopdf`. If that fails, round-trip through
pdfium (decompresses cross-reference streams and object streams to a form
lopdf can handle) and reload. Apply overlays as a separate appended
content stream, sandwiched between `q ... Q` so any non-identity CTM left
by the original content does not leak into the overlay (which would
otherwise mirror or displace the additions). Embed Inter once per saved
file as a Type0 font with Identity-H encoding and full glyph-width table.
Resource names (`/F1`, `/Im1`, …) are picked to avoid collisions with the
source PDF's existing resources. Save under a non-conflicting name.

## Known limitations

- Inter text is **not extractable** by `pdftotext` or by copy-paste in PDF
  readers, because no `ToUnicode` CMap is generated for the embedded font.
  The text renders correctly visually. If extractable text matters for
  your downstream workflow, this is a real gap; let me know if you need it.
- Page rotation (`/Rotate 90/180/270`) is not handled — overlays would land
  in the pre-rotation coordinate space on rotated pages. Most signed PDFs
  are upright; this hasn't bitten me yet.
- No undo. Use Ctrl+D for "experimental moves" — duplicate first, drag the
  copy, delete one of them when done.

## License

`pdfsigner` is licensed under [GPL-3.0-or-later](LICENSE). The bundled
Inter font is licensed under the
[SIL Open Font License 1.1](fonts/Inter-LICENSE.txt) and is redistributed
under that license.
