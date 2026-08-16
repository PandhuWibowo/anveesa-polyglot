# Anveesa Polyglot

A macOS overlay that live-translates whatever is on screen — e.g. a teammate's
Mandarin screen share — into a language you choose (English, Indonesian, …).

It runs in the background: every few seconds it captures the window or screen
you picked, OCRs the text on-device with Apple's Vision framework, and if the
text changed, translates it through an OpenAI-compatible API (DeepSeek by
default) and shows the result in an always-on-top panel.

## Requirements

- macOS 13+ (Apple Silicon or Intel)
- Rust toolchain + Xcode Command Line Tools (`clang` compiles the small
  Objective-C Vision OCR helper in `ocr/main.m` automatically via `build.rs`)
- An API key for an OpenAI-compatible endpoint

## Setup

Edit `config.toml` in the project root (gitignored — it holds your key):

```toml
api_base    = "https://ai.sumopod.com/v1"
api_key     = "sk-..."
model       = "deepseek-v4-pro"   # deepseek-v4-flash is faster, slightly rougher
target_lang = "Indonesian"        # any language name in plain words
interval_secs = 3.0               # capture cadence
```

All of these can also be changed live in the app (⚙ button), and saved from there.

## Run

```sh
cargo run --release
```

On first capture, macOS will ask for **Screen Recording** permission for your
terminal (System Settings → Privacy & Security → Screen Recording). Grant it
and restart the app.

Then in the overlay:
1. Pick the window or screen to watch in the **Capture** dropdown (🔄 refreshes the list).
2. Set **Translate to** (quick buttons for EN / ID, or type any language).
3. Keep the panel beside the shared screen. It re-translates only when the
   on-screen text changes; ⏸ pauses capturing.

## Live captions (Phase 2)

Click **🎤 Captions** in the app to live-translate speech from your speakers
(Zoom/Meet calls). Pipeline: an Objective-C helper (`audio/main.m`) captures
system audio via ScreenCaptureKit → whisper.cpp (Metal-accelerated, fully
on-device) transcribes Mandarin → each utterance is translated through the
same API and shown as caption pairs (original + translation).

Requires:
- the Whisper model at `whisper_model` (default
  `models/ggml-large-v3-turbo-q5_0.bin`, ~574 MB):
  ```sh
  mkdir -p models && curl -L -o models/ggml-large-v3-turbo-q5_0.bin \
    https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin
  ```
- `cmake` (`brew install cmake`) to build whisper.cpp the first time
- the same **Screen Recording** permission (system audio rides on it)

**Spoken languages**: in ⚙ Settings, pick as many languages as you expect to
hear (Mandarin, Japanese, Korean, English, …) — Whisper detects which one is
actually spoken per utterance, so mixed-language meetings just work, with no
real cap on how many you select. Pick exactly one only if every speaker uses
that language, for slightly better accuracy (a fixed hint vs. per-segment
detection). A "+ other code" field covers any of Whisper's ~99 supported
languages not in the quick list. Stored in `config.toml` as `stt_lang`,
comma-separated (e.g. `"zh,ja,ko"`) or `"auto"`.
Segments are cut on ~0.7 s pauses, capped at 8 s, so captions appear a
moment after someone stops speaking; translation fills in a few seconds later.

## File translation (Phase 3)

Click **📄 File** (or drag-and-drop a file onto the window) to translate a
whole file. Supported: anything text-based (txt, md, code, csv, json, …),
**PDF** (extracted with PDFKit via `doc/main.m`), and **docx / doc / rtf /
html / odt** (extracted with macOS's built-in `textutil`).

Long files are split on paragraph boundaries (~3000 chars per part) and
translated part by part with a progress bar; you can cancel mid-way and keep
the partial result. The result window has **Copy** and **Save next to
original** (e.g. `report.pdf` → `report.indonesian.txt`; plain-text files
keep their extension). Formatting, Markdown, and code are preserved — in code,
only comments and human-readable strings are translated.

## Spreadsheets: Excel & Google Sheets (Phase 4)

Drop (or 📄-pick) an **.xlsx / .xlsm** file and you get a translated **workbook**,
not a text dump: the file's shared-strings table is translated in place, so all
formatting, formulas, column widths, and sheets survive untouched. Output lands
next to the original (`inventory.xlsx` → `inventory.indonesian.xlsx`).
**.csv / .tsv** are translated cell by cell with structure preserved.

- Cell values are deduplicated and batch-translated (~40 per API call), with
  progress per batch and cancel support.
- Values without letters (numbers, dates) and code-like values (SKUs, IDs) are
  left unchanged.
- **Google Sheets**: File → Download → **Microsoft Excel (.xlsx)**, translate
  it here, then import back with File → Import in Sheets.
- Legacy **.xls** isn't supported — re-save it as .xlsx first.

## Right-click Quick View from Finder (Phase 5)

With Polyglot running in the background, **right-click any file in Finder →
Quick Actions → "Translate with Polyglot"** (sometimes under Services). Within
a few seconds the **Quick View** window shows the translation of the first
part of the file — **nothing is saved to disk**. It's for *reading*, on the fly:

- Documents (pdf/docx/md/txt/…) **stream** the translation in live, first
  words in ~1–2 s, using the `fast_model` (default
  `gemini/gemini-3.5-flash-lite`, ~2–4 s per part; any model on the endpoint
  works, e.g. `deepseek-v4-flash`).
- Spreadsheets show `original → translation` pairs for the first ~40 cell
  values (~5–7 s).
- **▸ Continue** translates the next part as you read.
- **💾 Translate all & save** is the only thing that writes a file — full
  quality (`model`), whole file, saved next to the original (real .xlsx for
  workbooks).

Mechanics: the Quick Action drops the file path into
`~/Library/Application Support/anveesa-polyglot/queue/`, which the app
watches. If the menu item doesn't appear: System Settings → General → Login
Items & Extensions → Extensions → Finder (enable it), or log out/in once.

## PDF translated in place, in the same file (Phase 9)

Open a PDF in Preview, then click **📕 PDF** in the Polyglot window. The app
finds the frontmost Preview document by itself, translates every
Chinese/Japanese/Korean line, and writes the translations **into that same
PDF** as positioned overlay annotations — each line covered by its
translation at the exact spot on the page. Preview is reloaded automatically;
you just watch the document turn Indonesian.

- The original text is **still in the file underneath** — the overlays are
  standard PDF annotations, removable in Preview (or by re-running, which
  replaces them). Copy/search still finds the original text.
- Re-clicking 📕 PDF re-translates cleanly (old overlays are replaced, never
  stacked).
- Only CJK lines are masked; existing English/Latin text is left visible.
- Uses `fast_model`, deduped and batched; a ~20-page book sample (358 lines)
  takes ~5 minutes.
- Headless: `cargo run -- --test-pdfmask file.pdf`

## Right-click inside a document: select text, translate (Phase 8)

The most seamless path: **select any text in any app** (Preview, Safari,
Word, a browser PDF viewer, anywhere) → **right-click → Services →
"Translate Selection with Polyglot"** (top-level on some macOS versions,
under a Services submenu on others). Quick View opens and streams the
translation of exactly what you selected — no target-picking, no separate
window to configure, no file involved. Long selections are chunked the same
way documents are, with **▸ Continue** for more; there's no "save" button
since there's no file, just **📋 Copy**.

This complements — doesn't replace — 📄 File (whole file) and 🎭 Mask
(everything visible on screen): select-and-translate is for "what does this
paragraph say", the fastest and most targeted of the three.

If it doesn't appear in the right-click menu: System Settings → Keyboard →
Keyboard Shortcuts → Services — the first time a new Service is installed,
macOS sometimes needs that pane opened once before it shows up in menus.

## Mask overlay: translate in place, over whatever's on screen (Phase 7)

Click **🎭 Mask** (needs a **Capture** target picked first, top bar) to cover
the original text on screen with an opaque box showing the translation, at
the exact position of the original — like the text was replaced in place.
Unlike Quick View/file translation, nothing is read from a file: it works on
anything rendered on screen (Preview, a browser, Slack, anything), live,
re-masking every `interval_secs` if the visible text changed.

- **Use the "🖥 Screen" target, not a specific window.** Picking an individual
  app's window is currently broken (`xcap`'s window enumeration doesn't see
  other apps' windows on this macOS version) — Screen capture works and is
  the practical option; OCR only reports boxes where it finds text, so it
  naturally masks just the relevant regions.
- The target content must be **actually visible** — not hidden behind
  another window — since this is screen capture, not a live document read.
- Click 🎭 Mask again to turn it off.
- Uses `fast_model`, batch-translating all detected lines in one call per
  cycle (like the spreadsheet path) — fast, but note it translates
  *everything* visible with text, including UI chrome, not just the document
  you care about.
- The overlay's background is the **actual captured frame**, not a live
  see-through window — true OS-level window transparency across viewports
  turned out to be unreliable in practice (worked in some window
  arrangements, rendered solid black in others, depending on what else was
  on screen). Painting the real screenshot behind the translated boxes is
  correct every time; the tradeoff is the backdrop only refreshes once per
  `interval_secs` instead of being truly live — if you scroll mid-cycle, the
  overlay catches up on the next capture.

## Live in-place translation for an open Numbers file (Phase 6)

Open a spreadsheet in **Numbers**, click its tab so it's the active sheet,
then click **🔢 Live Sheet** in the Polyglot window. Every text cell on that
sheet is translated and rewritten **in the document you're already looking
at** — no new file, no export. Each cell becomes:

```
原文
Translation
```

(original stacked above the translation, via a soft line break inside the
cell). Formula cells, numbers, and empty cells are left untouched.

- This is a real edit to the open document, driven by AppleScript — it is
  **not** saved to disk unless you save in Numbers. **Cmd+Z undoes it** like
  any other edit; closing without saving discards it entirely.
- Works one sheet at a time (matches how you read a workbook — click a tab,
  hit 🔢 Live Sheet again for that tab).
- Clicking again on an already-translated sheet skips cells that already
  look stacked, so it won't double up.
- Uses `fast_model` for speed. A ~60×6 sheet with ~15 unique phrases takes
  under 10 seconds.
- Numbers-only (that's what's installed here); Excel has an equivalent
  AppleScript dictionary if you ever need it, but isn't wired up.

## Debugging

Run any pipeline headless:

```sh
cargo run -- --test-pipeline screenshot.png   # OCR + translation
cargo run -- --test-audio speech.wav          # 16 kHz mono wav → STT + translation
cargo run -- --test-file document.pdf        # file extraction + translation
cargo run -- --test-sheet data.xlsx          # spreadsheet → translated workbook
cargo run -- --test-numbers                  # live-translate the active Numbers sheet
cargo run -- --test-capture                  # check Screen Recording permission
cargo run -- --test-mask "<window title substring>"   # one mask cycle, prints positions
```

## Roadmap

- Microphone input for in-room conversations (currently system audio only)
- Region selection (translate only part of a window)
- Overlay of translations positioned over the original text (OCR bounding
  boxes are already emitted by the helper)
