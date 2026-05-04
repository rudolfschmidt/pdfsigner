# pdfsigner

A minimal desktop PDF signer for Linux. Drop a PDF, place a signature image
and a date, save. Single binary, no cloud, no account, no proprietary stack.

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
  signatures directory
- Inserts text labels with the bundled Inter font, pickable color and size
- Quick-insert buttons for today's date in German (`04.05.2026`) and US
  (`05/04/2026`) format, plus an `X` button for checkboxes
- Edit text content of existing labels in-place
- Multi-selection via rubber-band drag (or Ctrl+click), bulk move and
  bulk delete
- Saves the result next to the original as `<name>_signed.pdf`
  (auto-numbered if the file exists)
- Optional page-range filter at save time (`1-3,5,7-9`) to extract only
  selected pages into the signed output
- Mouse-wheel page navigation
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

### Toolbar

- **Insert signature** — dropdown of files from
  `~/.config/pdfsigner/signatures/`. Selecting an entry inserts it onto the
  current page.
- **DE** / **US** — insert today's date in the chosen format
- **X** — insert an "X" character (for ticking checkboxes)
- **Text** field + **Add** — insert a custom text label. While a text label
  is selected the field switches to editing that label's content.
- **Size**, **color** — applied to the next-inserted text or to the
  currently-selected text label
- **Width** — appears when an image is selected, scales it preserving aspect
  ratio
- **Pages** — page-range filter (e.g. `1-3,5`); empty means save all pages
- **Save** (or Ctrl+S) — writes `<name>_signed.pdf` next to the original

### Selection and movement

- Click an overlay to select it
- Ctrl+click adds/removes from the selection
- Drag from empty space to draw a rubber-band rectangle and select all
  overlays inside it
- Drag a selected overlay to move it; if multiple are selected they all move
  together
- Delete (or Backspace) removes all selected overlays
- Ctrl+D duplicates all selected overlays at a small offset
- Mouse wheel changes page (one notch = one page)

### Output naming

Saving `~/Documents/contract.pdf` writes `~/Documents/contract_signed.pdf`.
If that file already exists, the next save uses `contract_signed_1.pdf`,
then `_2.pdf`, etc. The original PDF is never touched.

## Configuration

### Signatures directory

`~/.config/pdfsigner/signatures/` is created on first launch. Drop your
signature PNG or JPG files there and they appear in the toolbar dropdown
the next time you start the app. Only one level deep; no recursion.

For transparency, use a PNG with an alpha channel — the embedder writes a
soft mask so the signature blends with whatever is underneath in the PDF.

### Fonts

Inter Regular is bundled into the binary, no additional installation needed.
Bold/italic are not bundled (the toggle was removed because they would
not visibly differ).

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

A single Rust binary, ~1000 lines:

- **eframe / egui** — immediate-mode UI rendered via OpenGL, no system GUI
  toolkit dependency
- **pdfium-render** — PDF rendering for the editor view and structural
  normalization fallback
- **lopdf** — PDF object/stream construction for the overlay layer (text,
  images, fonts)
- **ttf-parser** — read Inter's TrueType metrics for embedding as a Type0
  CIDFontType2 PDF font
- **image** + **flate2** — image decoding and PDF stream compression
- **chrono** — date formatting for the quick-insert buttons

Save flow: load original via `lopdf`. If that fails, round-trip through
pdfium (decompresses cross-reference streams and object streams to a form
lopdf can handle) and reload. Apply overlays as a separate appended
content stream, sandwiched between `q ... Q` so any non-identity CTM left
by the original content does not leak into the overlay (which would
otherwise mirror or displace the additions). Embed Inter once per saved
file as a Type0 font with Identity-H encoding and full glyph-width table.
Save under a non-conflicting name.

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
