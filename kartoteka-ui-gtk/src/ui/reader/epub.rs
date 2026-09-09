//! The EPUB reader: chapter navigation, whole-book search, and highlight capture injected
//! into a WebKit view.
//!
//! An EPUB has no fixed page grid, so positions are chapter index plus a scroll fraction
//! within it (`fond_bib::Progress`). Persistence goes through [`ReaderHost`].

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gdk, gio, glib, Orientation};
use libadwaita as adw;
use libadwaita::prelude::*;
use webkit6::prelude::*;

use super::pdf::{COLOR_PRESETS, EPUB_MARK_KIND_OPTIONS, UNDO_HISTORY_LIMIT};
use crate::ui::reader::RebuildCell;
use crate::ui::reader::{self, popover_button, popover_separator, ReaderHost};

/// Live state of an open EPUB reader window: the chapter list, current position, and this
/// entry's annotation sidecar (loaded once at open and rewritten to disk on every highlight
/// added — the same "hold it in memory, don't re-read the library each time" approach the
/// PDF reader's `ReaderState` uses).
struct EpubReaderState {
    /// Where this EPUB's contents were extracted to (content-addressed by attachment hash,
    /// so a repeat "Read" reuses the extraction rather than re-unzipping every time).
    cache_dir: PathBuf,
    /// Chapter files in reading order, as paths relative to `cache_dir` (same values as
    /// `fond_doc::EpubBook::spine`).
    spine: Vec<String>,
    index: usize,
    annotations: fond_bib::AnnotationSidecar,
    /// Snapshot-based undo/redo, same idiom as the PDF reader's `ReaderState` (see
    /// `push_undo_snapshot`/`UNDO_HISTORY_LIMIT`) — a full clone of `annotations` taken
    /// immediately before each mutation (add/edit/delete a mark).
    undo_stack: Vec<fond_bib::AnnotationSidecar>,
    redo_stack: Vec<fond_bib::AnnotationSidecar>,
    /// Whole-book plain-text search index, one entry per `spine` chapter — built lazily
    /// (see `epub_chapter_texts`) the first time whole-book search is used, from the
    /// chapters already sitting in `cache_dir` (no re-opening the EPUB zip needed). `None`
    /// until then; cheap to keep in memory afterward for the life of this reader window.
    chapter_texts: Option<Vec<String>>,
}

/// Return this reader's per-chapter plain-text search index, building and caching it on
/// first use by reading each `spine` chapter straight from `cache_dir` (already extracted
/// when the reader opened) and stripping tags via `fond_doc::strip_epub_tags` — the same
/// tag-stripping `fond_doc::extract_epub_text` uses for the library-wide search index, just
/// kept per-chapter here instead of joined into one string, so a match can be attributed to
/// a chapter to jump to.
fn epub_chapter_texts(state: &Rc<RefCell<EpubReaderState>>) -> Vec<String> {
    if let Some(texts) = &state.borrow().chapter_texts {
        return texts.clone();
    }
    let (cache_dir, spine) = {
        let r = state.borrow();
        (r.cache_dir.clone(), r.spine.clone())
    };
    let texts: Vec<String> = spine
        .iter()
        .map(|chapter| {
            std::fs::read_to_string(cache_dir.join(chapter))
                .map(|xml| fond_doc::strip_epub_tags(&xml))
                .unwrap_or_default()
        })
        .collect();
    state.borrow_mut().chapter_texts = Some(texts.clone());
    texts
}

/// One whole-book search hit: which chapter (`spine` index) it's in, and a short excerpt of
/// surrounding context with the match itself in the returned range (`match_start`/`match_end`,
/// byte offsets into `snippet`) so the results list can bold it.
struct EpubWholeBookMatch {
    chapter: usize,
    snippet: String,
    match_start: usize,
    match_end: usize,
}

/// Search every chapter's plain-text index for `query` (case-insensitive substring), capped
/// at `EPUB_WHOLE_BOOK_MATCH_LIMIT` total hits so a common word in a long book doesn't build
/// an unbounded results list. Each hit carries ~50 characters of context on each side of the
/// match, trimmed to whitespace boundaries where possible so excerpts don't start/end mid-word.
const EPUB_WHOLE_BOOK_MATCH_LIMIT: usize = 200;

fn epub_search_whole_book(
    state: &Rc<RefCell<EpubReaderState>>,
    query: &str,
) -> Vec<EpubWholeBookMatch> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let texts = epub_chapter_texts(state);
    let needle = query.to_lowercase();
    let mut results = Vec::new();
    'chapters: for (chapter, text) in texts.iter().enumerate() {
        let haystack = text.to_lowercase();
        let mut search_from = 0;
        while let Some(rel_idx) = haystack[search_from..].find(&needle) {
            let idx = search_from + rel_idx;
            let end = idx + needle.len();
            let ctx_start = text[..idx]
                .char_indices()
                .rev()
                .take(50)
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            let ctx_end = text[end..]
                .char_indices()
                .nth(50)
                .map(|(i, _)| end + i)
                .unwrap_or(text.len());
            results.push(EpubWholeBookMatch {
                chapter,
                snippet: text[ctx_start..ctx_end].to_string(),
                match_start: idx - ctx_start,
                match_end: end - ctx_start,
            });
            if results.len() >= EPUB_WHOLE_BOOK_MATCH_LIMIT {
                break 'chapters;
            }
            search_from = end;
        }
    }
    results
}

/// Snapshot `reader`'s current annotations onto the undo stack and clear the redo stack —
/// same convention as the PDF reader's `push_undo_snapshot`, just typed to `EpubReaderState`.
fn push_epub_undo_snapshot(reader: &Rc<RefCell<EpubReaderState>>) {
    let mut r = reader.borrow_mut();
    let snapshot = r.annotations.clone();
    r.undo_stack.push(snapshot);
    if r.undo_stack.len() > UNDO_HISTORY_LIMIT {
        r.undo_stack.remove(0);
    }
    r.redo_stack.clear();
}

fn sync_epub_undo_redo_buttons(
    reader: &Rc<RefCell<EpubReaderState>>,
    undo_button: &gtk4::Button,
    redo_button: &gtk4::Button,
) {
    let r = reader.borrow();
    undo_button.set_sensitive(!r.undo_stack.is_empty());
    redo_button.set_sensitive(!r.redo_stack.is_empty());
}

/// One annotation as sent to the reader's highlight-apply JS: just enough to find it in the
/// rendered DOM (`snippet`, plus `prefix`/`suffix` context to disambiguate a snippet that
/// appears more than once in the chapter) and mark it (`id`, for the CSS class and as the
/// optional scroll target).
#[derive(serde::Serialize)]
struct EpubHighlightPayload<'a> {
    id: &'a str,
    snippet: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suffix: Option<&'a str>,
    /// Serializes lowercase (`"highlight"`/`"underline"`/`"strikeout"`) via
    /// `AnnotationKind`'s own `Serialize` impl — `EPUB_APPLY_HIGHLIGHTS_FN` switches on
    /// this to decide which CSS treatment to apply.
    kind: fond_bib::AnnotationKind,
    /// Highlight colour (hex, e.g. `#f6c344`) — meaningless for underline/strikeout,
    /// which always use the current text colour so they read correctly in dark mode too.
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<&'a str>,
}

/// What the reader's selection-capture JS reports back: either nothing was meaningfully
/// selected, or the selected text plus up to 40 characters of surrounding context on each
/// side — the same prefix/suffix disambiguation scheme the PDF sidecar already uses.
#[derive(serde::Deserialize)]
struct EpubSelectionCapture {
    empty: bool,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    suffix: Option<String>,
}

/// A JS function *expression* (no trailing call — callers append `(args)`) that finds each
/// given annotation's snippet text in the current document and wraps it in a
/// `<mark class="kartoteka-hl">`, clearing any marks left by a previous call first
/// (idempotent re-apply, so adding a highlight can just re-run this instead of reloading the
/// page). Scrolls the mark matching `scrollToId` into view, if given. Mirrors
/// `select_text_in_rect`'s multi-node-aware approach in `fond-doc/src/pdf.rs` — walk every
/// text node, find the target substring, map it back onto node+offset pairs — just over DOM
/// text nodes instead of PDF characters, since there's no PDFium text layer here.
const EPUB_APPLY_HIGHLIGHTS_FN: &str = r#"(function(annotations, scrollToId) {
  function findTextRange(root, snippet, prefix, suffix) {
    if (!snippet) return null;
    var walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, null);
    var nodes = [];
    var fullText = '';
    var node;
    while (node = walker.nextNode()) {
      nodes.push({ node: node, start: fullText.length });
      fullText += node.textContent;
    }
    var combined = (prefix || '') + snippet + (suffix || '');
    var idx, endIdx;
    var combinedIdx = combined.length > snippet.length ? fullText.indexOf(combined) : -1;
    if (combinedIdx !== -1) {
      idx = combinedIdx + (prefix || '').length;
      endIdx = idx + snippet.length;
    } else {
      var plainIdx = fullText.indexOf(snippet);
      if (plainIdx === -1) return null;
      idx = plainIdx;
      endIdx = idx + snippet.length;
    }
    var startNode = null, startOffset = 0, endNode = null, endOffset = 0;
    for (var i = 0; i < nodes.length; i++) {
      var n = nodes[i];
      var nEnd = n.start + n.node.textContent.length;
      if (startNode === null && idx >= n.start && idx < nEnd) {
        startNode = n.node; startOffset = idx - n.start;
      }
      if (endIdx > n.start && endIdx <= nEnd) {
        endNode = n.node; endOffset = endIdx - n.start;
      }
    }
    if (!startNode || !endNode) return null;
    var range = document.createRange();
    range.setStart(startNode, startOffset);
    range.setEnd(endNode, endOffset);
    return range;
  }

  document.querySelectorAll('mark.kartoteka-hl').forEach(function(m) {
    var parent = m.parentNode;
    if (!parent) return;
    while (m.firstChild) parent.insertBefore(m.firstChild, m);
    parent.removeChild(m);
    parent.normalize();
  });

  annotations.forEach(function(a) {
    var range = findTextRange(document.body, a.snippet, a.prefix, a.suffix);
    if (!range) return;
    var mark = document.createElement('mark');
    mark.className = 'kartoteka-hl';
    mark.dataset.annotationId = a.id;
    mark.dataset.kind = a.kind;
    // No background/foreground colour is hardcoded beyond the highlight tint itself
    // (which is the whole point of a highlight) — underline/strikeout use `currentColor`
    // so they read correctly against the page's own text colour in light or dark mode.
    if (a.kind === 'underline') {
      mark.style.background = 'transparent';
      mark.style.textDecoration = 'underline';
      mark.style.textDecorationColor = a.color || 'currentColor';
      mark.style.textDecorationThickness = '2px';
    } else if (a.kind === 'strikeout') {
      mark.style.background = 'transparent';
      mark.style.textDecoration = 'line-through';
      mark.style.textDecorationColor = a.color || 'currentColor';
    } else {
      mark.style.backgroundColor = a.color ? (a.color + '59') : 'rgba(246, 195, 68, 0.35)';
    }
    try {
      range.surroundContents(mark);
    } catch (e) {
      var contents = range.extractContents();
      mark.appendChild(contents);
      range.insertNode(mark);
    }
    if (scrollToId && a.id === scrollToId) {
      mark.scrollIntoView({ block: 'center' });
    }
  });
})"#;

/// JS that reports the `WebView`'s current text selection (if any) as JSON: `{empty: true}`
/// if nothing is meaningfully selected, else `{empty: false, text, prefix, suffix}` — the
/// selected text plus up to 40 characters of context on each side, computed by walking
/// `document.body`'s text nodes to find the selection's absolute character offset (the same
/// approach `EPUB_APPLY_HIGHLIGHTS_FN` uses in reverse, to re-locate a snippet later).
const EPUB_CAPTURE_SELECTION_JS: &str = r#"(function() {
  var sel = window.getSelection();
  if (!sel || sel.rangeCount === 0 || !sel.toString().trim()) {
    return JSON.stringify({ empty: true });
  }
  var text = sel.toString();
  var range = sel.getRangeAt(0);
  function textOffsetOf(node, offset) {
    var walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, null);
    var total = 0, n;
    while (n = walker.nextNode()) {
      if (n === node) return total + offset;
      total += n.textContent.length;
    }
    return total;
  }
  var fullText = document.body.textContent;
  var startIdx = textOffsetOf(range.startContainer, range.startOffset);
  var prefix = fullText.slice(Math.max(0, startIdx - 40), startIdx);
  var suffix = fullText.slice(startIdx + text.length, startIdx + text.length + 40);
  sel.removeAllRanges();
  return JSON.stringify({ empty: false, text: text, prefix: prefix, suffix: suffix });
})()"#;

/// Serialize the annotations anchored to `chapter` into the JSON array
/// `EPUB_APPLY_HIGHLIGHTS_FN` expects. An annotation with no `snippet` (shouldn't happen for
/// an EPUB one — `drawn_epub` always sets it — but the field is `Option` since the type is
/// shared with PDF annotations) is skipped rather than sent as an unfindable empty search.
fn epub_highlight_payload_json(sidecar: &fond_bib::AnnotationSidecar, chapter: &str) -> String {
    let items: Vec<EpubHighlightPayload> = sidecar
        .annotations
        .iter()
        .filter(|a| a.chapter.as_deref() == Some(chapter))
        .filter_map(|a| {
            a.snippet.as_deref().map(|s| EpubHighlightPayload {
                id: &a.id,
                snippet: s,
                prefix: a.snippet_prefix.as_deref(),
                suffix: a.snippet_suffix.as_deref(),
                kind: a.kind,
                color: a.color.as_deref(),
            })
        })
        .collect();
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
}

/// Run `EPUB_APPLY_HIGHLIGHTS_FN` against the currently-loaded chapter, scrolling
/// `scroll_to_id`'s mark into view if given. Called after every chapter load (so navigating
/// away and back keeps showing highlights) and right after adding a new highlight (so it
/// appears immediately, no reload needed).
fn epub_apply_highlights(
    view: &webkit6::WebView,
    state: &Rc<RefCell<EpubReaderState>>,
    scroll_to_id: Option<&str>,
) {
    let payload_json = {
        let r = state.borrow();
        match r.spine.get(r.index) {
            Some(chapter) => epub_highlight_payload_json(&r.annotations, chapter),
            None => return,
        }
    };
    let scroll_json = match scroll_to_id {
        Some(id) => serde_json::to_string(id).unwrap_or_else(|_| "null".to_string()),
        None => "null".to_string(),
    };
    let script = format!("{EPUB_APPLY_HIGHLIGHTS_FN}({payload_json}, {scroll_json})");
    view.evaluate_javascript(&script, None, None, gio::Cancellable::NONE, |_| {});
}

/// A built-in EPUB reader: renders each chapter with WebKitGTK (`webkit6`), which — unlike
/// PDFium for the PDF reader — handles an XHTML+CSS chapter's layout, images, and text
/// selection natively, so this only has to handle chapter/TOC navigation plus highlighting.
/// `hash` is the attachment's content hash (`blake3:…`), used to pick a stable extraction
/// cache directory — the EPUB zip is extracted to disk once per unique file (not re-unzipped
/// on every "Read") so chapter-relative links to sibling images/CSS resolve the normal
/// browser way over `file://`, rather than needing a custom URI scheme handler.
///
/// Highlighting (M5-SPEC.md 5C) reuses the `WebView`'s own native text selection — the user
/// drags to select the ordinary browser way, then "Highlight" captures it via
/// `EPUB_CAPTURE_SELECTION_JS` and saves a `fond_bib::Annotation::drawn_epub` (chapter +
/// snippet + context; no page/quadpoints — there's no fixed page grid to hang those on) into
/// the same `annots/<key>.json` sidecar the PDF reader writes. Applying saved highlights back
/// onto the page is a live DOM search-and-wrap (`epub_apply_highlights`) run after every
/// chapter load, not a raster blend like the PDF reader's `blend_highlights` — there's no
/// bitmap to blend into here, WebKit owns the actual rendering.
///
/// `start_annotation_id`, if given, opens on that annotation's chapter and scrolls it into
/// view once highlights are applied — the EPUB equivalent of the PDF reader's `start_page`.
/// `start_progress`, if given (and `start_annotation_id` isn't — a specific annotation jump
/// always wins), resumes at the entry's saved reading position: `progress.page - 1` as the
/// starting chapter index, then `progress.chapter_percent` (if set) as a scroll-fraction
/// restore once that chapter finishes loading. The EPUB equivalent of the PDF reader's own
/// `start_page` resume, using the chapter+percent shape `fond_bib::Progress` gained for it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show_epub_reader(
    host: &Rc<dyn ReaderHost>,
    window: &adw::ApplicationWindow,
    hash: &str,
    blob: &std::path::Path,
    title: &str,
    start_annotation_id: Option<&str>,
    start_progress: Option<fond_bib::Progress>,
) {
    // Already open? Surface it instead of opening a duplicate reader on the same file — see
    // the identical check (and `reader::OPEN_READERS`'s doc comment) in `show_pdf_reader`.
    if reader::present_existing(hash) {
        return;
    }

    let book = match fond_doc::open_book(blob) {
        Ok(b) => b,
        Err(e) => {
            gtk4::AlertDialog::builder()
                .message("Could not open EPUB")
                .detail(e.to_string())
                .build()
                .show(Some(window));
            return;
        }
    };

    let hex = hash.split_once(':').map(|(_, h)| h).unwrap_or(hash);
    let cache_dir = glib::user_cache_dir()
        .join("kartoteka")
        .join("epub")
        .join(hex);
    if !cache_dir.exists() {
        let extracted = std::fs::create_dir_all(&cache_dir)
            .map_err(|e| e.to_string())
            .and_then(|_| fond_doc::extract_epub(blob, &cache_dir).map_err(|e| e.to_string()));
        if let Err(e) = extracted {
            gtk4::AlertDialog::builder()
                .message("Could not open EPUB")
                .detail(e)
                .build()
                .show(Some(window));
            return;
        }
    }

    let annotations = host.load_annotations();

    let start_index = start_annotation_id
        .and_then(|id| annotations.annotations.iter().find(|a| a.id == id))
        .and_then(|a| a.chapter.as_deref())
        .and_then(|chapter| book.spine.iter().position(|p| p == chapter))
        .or_else(|| {
            start_progress.and_then(|p| {
                let idx = (p.page as usize).saturating_sub(1);
                (idx < book.spine.len()).then_some(idx)
            })
        })
        .unwrap_or(0);
    // Only restore the scroll-within-chapter fraction when it's actually this chapter we're
    // opening on — a stale percent from a since-shrunk book, or a jump that landed on a
    // different chapter than `start_progress` recorded, would scroll to the wrong spot.
    let start_percent = start_annotation_id
        .is_none()
        .then(|| start_progress.filter(|p| (p.page as usize).saturating_sub(1) == start_index))
        .flatten()
        .and_then(|p| p.chapter_percent);

    let reader = Rc::new(RefCell::new(EpubReaderState {
        cache_dir,
        spine: book.spine,
        index: start_index,
        annotations,
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
        chapter_texts: None,
    }));

    let dialog = adw::Window::new();
    dialog.set_title(Some(title));
    dialog.set_transient_for(Some(window));
    dialog.set_default_size(1000, 820);
    reader::register_window(hash, &dialog);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.add_css_class("fond-chrome");

    let prev = gtk4::Button::from_icon_name("go-previous-symbolic");
    prev.set_tooltip_text(Some("Previous chapter"));
    let next = gtk4::Button::from_icon_name("go-next-symbolic");
    next.set_tooltip_text(Some("Next chapter"));
    let chapter_label = gtk4::Label::new(None);
    chapter_label.add_css_class("dim-label");
    let nav = gtk4::Box::new(Orientation::Horizontal, 6);
    nav.append(&prev);
    nav.append(&chapter_label);
    nav.append(&next);
    header.set_title_widget(Some(&nav));

    let web_view = webkit6::WebView::new();
    web_view.set_vexpand(true);
    web_view.set_hexpand(true);
    // Scale text size only, not the page layout/images — "zoom" on a WebView otherwise
    // scales everything, which reads as zooming a picture rather than adjusting font size.
    if let Some(settings) = webkit6::prelude::WebViewExt::settings(&web_view) {
        settings.set_zoom_text_only(true);
    }

    let hint = gtk4::Label::new(Some("Select text, choose a kind, then click Apply"));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    hint.set_margin_top(4);
    hint.set_margin_bottom(4);

    // Search: WebKit's own `FindController` for in-chapter search (highlights/cycles matches
    // in the currently loaded chapter, same as Ctrl+F in a browser) — plus a whole-book mode
    // (`whole_book_toggle`) that searches every chapter's plain-text index
    // (`epub_search_whole_book`) and lists results to jump to, since `FindController` itself
    // only ever sees the one chapter that's actually loaded.
    let search_toggle = gtk4::ToggleButton::new();
    search_toggle.set_icon_name("edit-find-symbolic");
    search_toggle.set_tooltip_text(Some("Search (Ctrl+F)"));
    let search_bar_entry = gtk4::SearchEntry::new();
    search_bar_entry.set_placeholder_text(Some("Search this chapter"));
    search_bar_entry.set_hexpand(true);
    let search_prev = gtk4::Button::from_icon_name("go-up-symbolic");
    search_prev.set_tooltip_text(Some("Previous match"));
    let search_next = gtk4::Button::from_icon_name("go-down-symbolic");
    search_next.set_tooltip_text(Some("Next match"));
    let whole_book_toggle = gtk4::ToggleButton::with_label("Whole book");
    whole_book_toggle.add_css_class("flat");
    whole_book_toggle.set_tooltip_text(Some(
        "Search every chapter instead of just the one currently open",
    ));
    let search_count = gtk4::Label::new(None);
    search_count.add_css_class("dim-label");
    search_count.add_css_class("caption");
    let search_row = gtk4::Box::new(Orientation::Horizontal, 6);
    search_row.set_margin_top(6);
    search_row.set_margin_start(8);
    search_row.set_margin_end(8);
    search_row.append(&search_bar_entry);
    search_row.append(&search_count);
    search_row.append(&search_prev);
    search_row.append(&search_next);
    search_row.append(&whole_book_toggle);

    // Whole-book results: chapter + excerpt, the match bolded via Pango markup. Only shown
    // (and only populated) while `whole_book_toggle` is active.
    let results_list = gtk4::ListBox::new();
    results_list.set_selection_mode(gtk4::SelectionMode::None);
    let results_scroll = gtk4::ScrolledWindow::new();
    results_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    results_scroll.set_min_content_height(160);
    results_scroll.set_max_content_height(240);
    results_scroll.set_propagate_natural_height(true);
    results_scroll.set_child(Some(&results_list));
    let results_revealer = gtk4::Revealer::new();
    results_revealer.set_reveal_child(false);
    results_revealer.set_child(Some(&results_scroll));

    let search_container = gtk4::Box::new(Orientation::Vertical, 0);
    search_container.append(&search_row);
    search_container.append(&results_revealer);
    let search_revealer = gtk4::Revealer::new();
    search_revealer.set_reveal_child(false);
    search_revealer.set_child(Some(&search_container));

    let content = gtk4::Box::new(Orientation::Vertical, 0);
    content.append(&hint);
    content.append(&search_revealer);
    content.append(&web_view);

    // Sidebar toggles (Contents, if the EPUB has a TOC; Notes always) — persistent Paned
    // sidebar, not popovers, matching the PDF reader's own house sidebar style (see
    // `show_pdf_reader`'s `sidebar_toggle`/`notes_toggle` pair, and CLAUDE.md's UI
    // standard). `Apply`/mode/colour stay at the end of the header, same relative position
    // "Highlight" used to occupy.
    let sidebar_toggle = (!book.toc.is_empty()).then(|| {
        let button = gtk4::ToggleButton::new();
        button.set_icon_name("sidebar-show-symbolic");
        button.set_tooltip_text(Some("Show the table of contents"));
        button
    });
    let notes_toggle = gtk4::ToggleButton::new();
    notes_toggle.set_icon_name("view-list-symbolic");
    notes_toggle.set_tooltip_text(Some("Show notes and highlights"));

    // Undo/redo: same snapshot-based idiom as the PDF reader (see `EpubReaderState`'s
    // `undo_stack`/`redo_stack` and `push_epub_undo_snapshot`).
    let undo_button = gtk4::Button::from_icon_name("edit-undo-symbolic");
    undo_button.set_tooltip_text(Some("Undo (Ctrl+Z)"));
    undo_button.set_sensitive(false);
    let redo_button = gtk4::Button::from_icon_name("edit-redo-symbolic");
    redo_button.set_tooltip_text(Some("Redo (Ctrl+Shift+Z)"));
    redo_button.set_sensitive(false);

    let mode_labels: Vec<&str> = EPUB_MARK_KIND_OPTIONS.iter().map(|(l, _)| *l).collect();
    let mode_drop = gtk4::DropDown::from_strings(&mode_labels);
    mode_drop.set_tooltip_text(Some("What kind of mark to apply to the selection"));
    let color_labels: Vec<&str> = COLOR_PRESETS.iter().map(|(l, _)| *l).collect();
    let color_drop = gtk4::DropDown::from_strings(&color_labels);
    color_drop.set_tooltip_text(Some("Highlight colour"));
    let apply_button = gtk4::Button::with_label("Apply");
    apply_button.set_tooltip_text(Some("Mark the selected text"));

    // Font size: text-only zoom (see `set_zoom_text_only` above), stepped like the PDF
    // reader's own zoom buttons. Not persisted across sessions (unlike window/pane sizing)
    // — a book-length reading choice you're more likely to want to readjust per-book than
    // to lock in globally.
    let font_zoom: Rc<Cell<f64>> = Rc::new(Cell::new(1.0));
    let zoom_out_button = gtk4::Button::from_icon_name("zoom-out-symbolic");
    zoom_out_button.add_css_class("flat");
    zoom_out_button.set_tooltip_text(Some("Smaller text"));
    let zoom_in_button = gtk4::Button::from_icon_name("zoom-in-symbolic");
    zoom_in_button.add_css_class("flat");
    zoom_in_button.set_tooltip_text(Some("Larger text"));
    {
        let web_view = web_view.clone();
        let font_zoom = font_zoom.clone();
        zoom_out_button.connect_clicked(move |_| {
            let z = (font_zoom.get() - 0.1).max(0.5);
            font_zoom.set(z);
            web_view.set_zoom_level(z);
        });
    }
    {
        let web_view = web_view.clone();
        let font_zoom = font_zoom.clone();
        zoom_in_button.connect_clicked(move |_| {
            let z = (font_zoom.get() + 0.1).min(3.0);
            font_zoom.set(z);
            web_view.set_zoom_level(z);
        });
    }

    // pack_end order is the reverse of visual order (same gotcha CLAUDE.md notes for the
    // hamburger menu) — Apply packed first so it ends up rightmost: Mode, Colour, Apply,
    // Font size. Sidebar toggles, search, and undo/redo go at the header's start, per house
    // style (undo/redo in the same relative position as the PDF reader's own pair).
    header.pack_end(&apply_button);
    header.pack_end(&color_drop);
    header.pack_end(&mode_drop);
    header.pack_end(&zoom_in_button);
    header.pack_end(&zoom_out_button);
    if let Some(sidebar_toggle) = &sidebar_toggle {
        header.pack_start(sidebar_toggle);
    }
    header.pack_start(&notes_toggle);
    header.pack_start(&search_toggle);
    header.pack_start(&undo_button);
    header.pack_start(&redo_button);
    view.add_top_bar(&header);

    // Contents sidebar (only built if the EPUB has a TOC).
    let contents_scroll = sidebar_toggle.as_ref().map(|_| {
        let rows = gtk4::Box::new(Orientation::Vertical, 2);
        rows.set_margin_top(6);
        rows.set_margin_bottom(6);
        rows.set_margin_start(6);
        rows.set_margin_end(6);
        let last = book.toc.len().saturating_sub(1);
        for (i, entry) in book.toc.iter().enumerate() {
            let row = popover_button(&entry.label, false);
            if let Some(lbl) = row.child().and_then(|w| w.downcast::<gtk4::Label>().ok()) {
                lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            }
            {
                let reader = reader.clone();
                let view = web_view.clone();
                let prev = prev.clone();
                let next = next.clone();
                let chapter_label = chapter_label.clone();
                let target = entry.target.clone();
                row.connect_clicked(move |_| {
                    epub_go_to(&reader, &view, &prev, &next, &chapter_label, &target);
                });
            }
            rows.append(&row);
            if i != last {
                rows.append(&popover_separator());
            }
        }
        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        scroll.set_child(Some(&rows));
        scroll
    });

    // Notes/highlights sidebar: every annotation on this EPUB, in reading order, readable
    // prose rather than just an in-text mark — same pattern as the PDF reader's own notes
    // sidebar (`show_pdf_reader`), rebuilt via the same self-referential-cell idiom so a
    // row's own delete button can trigger a fresh rebuild of the list it lives in.
    let notes_rows = gtk4::Box::new(Orientation::Vertical, 2);
    notes_rows.set_margin_top(6);
    notes_rows.set_margin_bottom(6);
    notes_rows.set_margin_start(6);
    notes_rows.set_margin_end(6);
    let notes_scroll = gtk4::ScrolledWindow::new();
    notes_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    notes_scroll.set_child(Some(&notes_rows));

    // Pending scroll target for the *next* chapter load — set right before calling
    // `epub_go_to` by anything that wants the freshly-loaded chapter to scroll to a
    // specific annotation (the initial `start_annotation_id`, or a notes-sidebar jump);
    // left `None` for plain prev/next/TOC navigation, which just lands at the top.
    let pending_scroll: Rc<RefCell<Option<String>>> =
        Rc::new(RefCell::new(start_annotation_id.map(|s| s.to_string())));

    // Reading-position resume: consumed by the very first `load-changed` finish (see below),
    // never set again afterward, so it can't fight a later prev/next/TOC/search jump.
    let pending_scroll_percent: Rc<RefCell<Option<u8>>> = Rc::new(RefCell::new(start_percent));

    // Whole-book search: set right before navigating to a result's chapter, so the
    // `load-changed` handler below can hand the query to WebKit's `FindController` once that
    // chapter has actually finished loading — highlighting/scrolling to the match the same
    // way in-chapter search already does, just arriving at the right chapter first.
    let pending_search: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    let rebuild_notes_cell: RebuildCell = Rc::new(RefCell::new(None));
    {
        let notes_rows = notes_rows.clone();
        let host = host.clone();
        let reader = reader.clone();
        let view = web_view.clone();
        let prev = prev.clone();
        let next = next.clone();
        let chapter_label = chapter_label.clone();
        let pending_scroll = pending_scroll.clone();
        let rebuild_notes_cell_inner = rebuild_notes_cell.clone();
        let builder = move || {
            while let Some(child) = notes_rows.first_child() {
                notes_rows.remove(&child);
            }
            let mut all: Vec<fond_bib::Annotation> = reader
                .borrow()
                .annotations
                .annotations
                .iter()
                .filter(|a| a.chapter.is_some())
                .cloned()
                .collect();
            all.sort_by_key(|a| {
                let r = reader.borrow();
                let spine_pos = a
                    .chapter
                    .as_deref()
                    .and_then(|c| r.spine.iter().position(|p| p == c))
                    .unwrap_or(usize::MAX);
                (spine_pos, a.created.clone())
            });
            if all.is_empty() {
                let label = gtk4::Label::new(Some("No notes or highlights yet"));
                label.add_css_class("dim-label");
                label.set_margin_top(6);
                label.set_margin_bottom(6);
                notes_rows.append(&label);
                return;
            }
            let last = all.len().saturating_sub(1);
            for (i, annotation) in all.into_iter().enumerate() {
                let Some(chapter) = annotation.chapter.clone() else {
                    continue;
                };
                let chapter_num = reader
                    .borrow()
                    .spine
                    .iter()
                    .position(|p| p == &chapter)
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let kind_label = match annotation.kind {
                    fond_bib::AnnotationKind::Highlight => "Highlight",
                    fond_bib::AnnotationKind::Underline => "Underline",
                    fond_bib::AnnotationKind::Strikeout => "Strikeout",
                    fond_bib::AnnotationKind::Note => "Note",
                };
                let outer = gtk4::Box::new(Orientation::Vertical, 2);

                let header_box = gtk4::Box::new(Orientation::Horizontal, 6);
                let header_label =
                    gtk4::Label::new(Some(&format!("Ch. {chapter_num} — {kind_label}")));
                header_label.set_xalign(0.0);
                header_label.set_hexpand(true);
                header_label.add_css_class("dim-label");
                header_label.add_css_class("caption-heading");
                header_box.append(&header_label);
                let delete_button = gtk4::Button::from_icon_name("user-trash-symbolic");
                delete_button.add_css_class("flat");
                delete_button.set_tooltip_text(Some("Delete this annotation"));
                header_box.append(&delete_button);
                outer.append(&header_box);

                {
                    let jump = gtk4::GestureClick::new();
                    let reader = reader.clone();
                    let view = view.clone();
                    let prev = prev.clone();
                    let next = next.clone();
                    let chapter_label = chapter_label.clone();
                    let pending_scroll = pending_scroll.clone();
                    let id = annotation.id.clone();
                    let chapter = chapter.clone();
                    jump.connect_released(move |_gesture, _n, _x, _y| {
                        *pending_scroll.borrow_mut() = Some(id.clone());
                        epub_go_to(&reader, &view, &prev, &next, &chapter_label, &chapter);
                    });
                    header_label.add_controller(jump);
                }

                if let Some(snippet) = &annotation.snippet {
                    let snippet_label = gtk4::Label::new(Some(snippet));
                    snippet_label.set_xalign(0.0);
                    snippet_label.set_wrap(true);
                    snippet_label.add_css_class("dim-label");
                    snippet_label.add_css_class("caption");
                    outer.append(&snippet_label);
                }

                let note_entry = gtk4::Entry::new();
                note_entry.set_placeholder_text(Some("No note"));
                if let Some(note) = &annotation.note {
                    note_entry.set_text(note);
                }
                outer.append(&note_entry);

                let save_note = {
                    let host = host.clone();
                    let reader = reader.clone();
                    let id = annotation.id.clone();
                    move |text: &str| {
                        let text = text.trim();
                        let current_note = reader
                            .borrow()
                            .annotations
                            .annotations
                            .iter()
                            .find(|a| a.id == id)
                            .and_then(|a| a.note.clone());
                        if current_note.as_deref().unwrap_or("") == text {
                            return;
                        }
                        push_epub_undo_snapshot(&reader);
                        {
                            let mut r = reader.borrow_mut();
                            if let Some(a) =
                                r.annotations.annotations.iter_mut().find(|a| a.id == id)
                            {
                                a.note = (!text.is_empty()).then(|| text.to_string());
                            }
                        }
                        if let Err(e) = host.save_annotations(&reader.borrow().annotations) {
                            host.notify(&e);
                        }
                    }
                };
                {
                    let save_note = save_note.clone();
                    note_entry.connect_activate(move |e| save_note(&e.text()));
                }
                {
                    let focus = gtk4::EventControllerFocus::new();
                    let save_note = save_note.clone();
                    let note_entry_weak = note_entry.downgrade();
                    focus.connect_leave(move |_| {
                        if let Some(e) = note_entry_weak.upgrade() {
                            save_note(&e.text());
                        }
                    });
                    note_entry.add_controller(focus);
                }

                {
                    let host = host.clone();
                    let reader = reader.clone();
                    let view = view.clone();
                    let id = annotation.id.clone();
                    let rebuild_notes_cell = rebuild_notes_cell_inner.clone();
                    delete_button.connect_clicked(move |_| {
                        push_epub_undo_snapshot(&reader);
                        reader
                            .borrow_mut()
                            .annotations
                            .annotations
                            .retain(|a| a.id != id);
                        let write_result = host.save_annotations(&reader.borrow().annotations);
                        match write_result {
                            Ok(()) => {
                                epub_apply_highlights(&view, &reader, None);
                                host.notify("Annotation deleted");
                                if let Some(f) = rebuild_notes_cell.borrow().as_ref() {
                                    f();
                                }
                            }
                            Err(e) => host.notify(&e),
                        }
                    });
                }

                notes_rows.append(&outer);
                if i != last {
                    notes_rows.append(&popover_separator());
                }
            }
        };
        *rebuild_notes_cell.borrow_mut() = Some(Rc::new(builder));
    }
    let rebuild_notes: Rc<dyn Fn()> = {
        let cell = rebuild_notes_cell.clone();
        Rc::new(move || {
            let f = cell.borrow().clone();
            if let Some(f) = f {
                f();
            }
        })
    };

    // Contents (left) and Notes (right) are now two independent sidebars rather than a
    // shared Stack behind one toggle slot — see `show_pdf_reader`'s matching sidebars for
    // the full rationale (both can be open at once instead of sharing one toggle slot).
    if let Some(contents_scroll) = &contents_scroll {
        contents_scroll.set_size_request(60, -1);
    }
    notes_scroll.set_size_request(60, -1);

    let notes_paned = gtk4::Paned::new(Orientation::Horizontal);
    notes_paned.set_start_child(Some(&content));
    notes_paned.set_resize_start_child(true);
    notes_paned.set_shrink_start_child(false);
    notes_paned.set_end_child(gtk4::Widget::NONE);
    notes_paned.set_resize_end_child(false);
    notes_paned.set_shrink_end_child(true);
    notes_paned.set_vexpand(true);
    notes_paned.set_hexpand(true);

    let paned = gtk4::Paned::new(Orientation::Horizontal);
    paned.set_start_child(gtk4::Widget::NONE);
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(true);
    paned.set_end_child(Some(&notes_paned));
    paned.set_vexpand(true);
    paned.set_hexpand(true);
    paned.set_position(220);
    view.set_content(Some(&paned));
    dialog.set_content(Some(&view));

    // Undo/redo: pop a snapshot and refresh the current chapter's highlights plus the notes
    // sidebar — cheap, since there's no per-page render state to rebuild the way PDF's
    // continuous mode has.
    let epub_undo = {
        let reader = reader.clone();
        let host = host.clone();
        let view = web_view.clone();
        let rebuild_notes = rebuild_notes.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        Rc::new(move || {
            let popped = {
                let mut r = reader.borrow_mut();
                match r.undo_stack.pop() {
                    Some(prev) => {
                        let current = r.annotations.clone();
                        r.redo_stack.push(current);
                        r.annotations = prev;
                        true
                    }
                    None => false,
                }
            };
            if !popped {
                host.notify("Nothing to undo");
                return;
            }
            let write_result = host.save_annotations(&reader.borrow().annotations);
            match write_result {
                Ok(()) => {
                    epub_apply_highlights(&view, &reader, None);
                    rebuild_notes();
                    host.notify("Undid last annotation change");
                }
                Err(e) => host.notify(&format!("Could not undo: {e}")),
            }
            sync_epub_undo_redo_buttons(&reader, &undo_button, &redo_button);
        })
    };
    let epub_redo = {
        let reader = reader.clone();
        let host = host.clone();
        let view = web_view.clone();
        let rebuild_notes = rebuild_notes.clone();
        let undo_button = undo_button.clone();
        let redo_button = redo_button.clone();
        Rc::new(move || {
            let popped = {
                let mut r = reader.borrow_mut();
                match r.redo_stack.pop() {
                    Some(next) => {
                        let current = r.annotations.clone();
                        r.undo_stack.push(current);
                        r.annotations = next;
                        true
                    }
                    None => false,
                }
            };
            if !popped {
                host.notify("Nothing to redo");
                return;
            }
            let write_result = host.save_annotations(&reader.borrow().annotations);
            match write_result {
                Ok(()) => {
                    epub_apply_highlights(&view, &reader, None);
                    rebuild_notes();
                    host.notify("Redid annotation change");
                }
                Err(e) => host.notify(&format!("Could not redo: {e}")),
            }
            sync_epub_undo_redo_buttons(&reader, &undo_button, &redo_button);
        })
    };
    {
        let epub_undo = epub_undo.clone();
        undo_button.connect_clicked(move |_| epub_undo());
    }
    {
        let epub_redo = epub_redo.clone();
        redo_button.connect_clicked(move |_| epub_redo());
    }

    // Search wiring: toggling `search_toggle` reveals the bar and focuses the entry; turning
    // it off clears the query, WebKit's highlight state (`search_finish`), the whole-book
    // results list, and drops out of whole-book mode — so reopening search always starts
    // from the same clean state rather than leaving stale results/highlights visible.
    {
        let search_revealer = search_revealer.clone();
        let search_bar_entry = search_bar_entry.clone();
        let search_count = search_count.clone();
        let results_list = results_list.clone();
        let results_revealer = results_revealer.clone();
        let whole_book_toggle = whole_book_toggle.clone();
        let view = web_view.clone();
        search_toggle.connect_toggled(move |btn| {
            let on = btn.is_active();
            search_revealer.set_reveal_child(on);
            if on {
                search_bar_entry.grab_focus();
            } else {
                search_bar_entry.set_text("");
                search_count.set_text("");
                whole_book_toggle.set_active(false);
                results_revealer.set_reveal_child(false);
                while let Some(child) = results_list.first_child() {
                    results_list.remove(&child);
                }
                if let Some(fc) = webkit6::prelude::WebViewExt::find_controller(&view) {
                    fc.search_finish();
                }
            }
        });
    }
    {
        let search_bar_entry = search_bar_entry.clone();
        let search_count = search_count.clone();
        let results_list = results_list.clone();
        let results_revealer = results_revealer.clone();
        let view = web_view.clone();
        whole_book_toggle.connect_toggled(move |btn| {
            search_bar_entry.set_placeholder_text(Some(if btn.is_active() {
                "Search the whole book"
            } else {
                "Search this chapter"
            }));
            search_bar_entry.set_text("");
            search_count.set_text("");
            results_revealer.set_reveal_child(false);
            while let Some(child) = results_list.first_child() {
                results_list.remove(&child);
            }
            if let Some(fc) = webkit6::prelude::WebViewExt::find_controller(&view) {
                fc.search_finish();
            }
            search_bar_entry.grab_focus();
        });
    }
    {
        let view = web_view.clone();
        let search_count = search_count.clone();
        let whole_book_toggle = whole_book_toggle.clone();
        let results_list = results_list.clone();
        let results_revealer = results_revealer.clone();
        let reader = reader.clone();
        let prev = prev.clone();
        let next = next.clone();
        let chapter_label = chapter_label.clone();
        let pending_search = pending_search.clone();
        search_bar_entry.connect_search_changed(move |entry| {
            let text = entry.text();

            if whole_book_toggle.is_active() {
                while let Some(child) = results_list.first_child() {
                    results_list.remove(&child);
                }
                if text.is_empty() {
                    search_count.set_text("");
                    results_revealer.set_reveal_child(false);
                    return;
                }
                let matches = epub_search_whole_book(&reader, &text);
                if matches.is_empty() {
                    search_count.set_text("No matches");
                    results_revealer.set_reveal_child(false);
                    return;
                }
                search_count.set_text(&if matches.len() >= EPUB_WHOLE_BOOK_MATCH_LIMIT {
                    format!("{EPUB_WHOLE_BOOK_MATCH_LIMIT}+ found")
                } else {
                    format!("{} found", matches.len())
                });
                let spine_len = reader.borrow().spine.len();
                for m in &matches {
                    let before = glib::markup_escape_text(&m.snippet[..m.match_start]);
                    let hit = glib::markup_escape_text(&m.snippet[m.match_start..m.match_end]);
                    let after = glib::markup_escape_text(&m.snippet[m.match_end..]);
                    let row = gtk4::ListBoxRow::new();
                    let box_ = gtk4::Box::new(Orientation::Vertical, 2);
                    box_.set_margin_top(6);
                    box_.set_margin_bottom(6);
                    box_.set_margin_start(6);
                    box_.set_margin_end(6);
                    let heading = gtk4::Label::new(Some(&format!(
                        "Chapter {} of {}",
                        m.chapter + 1,
                        spine_len
                    )));
                    heading.set_xalign(0.0);
                    heading.add_css_class("dim-label");
                    heading.add_css_class("caption-heading");
                    box_.append(&heading);
                    let excerpt = gtk4::Label::new(None);
                    excerpt.set_markup(&format!("…{before}<b>{hit}</b>{after}…"));
                    excerpt.set_xalign(0.0);
                    excerpt.set_wrap(true);
                    excerpt.set_ellipsize(gtk4::pango::EllipsizeMode::None);
                    box_.append(&excerpt);
                    row.set_child(Some(&box_));

                    let click = gtk4::GestureClick::new();
                    let reader = reader.clone();
                    let view = view.clone();
                    let prev = prev.clone();
                    let next = next.clone();
                    let chapter_label = chapter_label.clone();
                    let pending_search = pending_search.clone();
                    let query = text.to_string();
                    let chapter_idx = m.chapter;
                    click.connect_released(move |_, _n_press, _x, _y| {
                        let target = {
                            let r = reader.borrow();
                            r.spine.get(chapter_idx).cloned()
                        };
                        let Some(target) = target else { return };
                        *pending_search.borrow_mut() = Some(query.clone());
                        epub_go_to(&reader, &view, &prev, &next, &chapter_label, &target);
                    });
                    row.add_controller(click);
                    results_list.append(&row);
                }
                results_revealer.set_reveal_child(true);
                return;
            }

            results_revealer.set_reveal_child(false);
            let Some(fc) = webkit6::prelude::WebViewExt::find_controller(&view) else {
                return;
            };
            if text.is_empty() {
                fc.search_finish();
                search_count.set_text("");
                return;
            }
            let options =
                (webkit6::FindOptions::CASE_INSENSITIVE | webkit6::FindOptions::WRAP_AROUND).bits();
            fc.search(&text, options, 1000);
        });
    }
    {
        let view = web_view.clone();
        let whole_book_toggle = whole_book_toggle.clone();
        search_prev.connect_clicked(move |_| {
            if whole_book_toggle.is_active() {
                return;
            }
            if let Some(fc) = webkit6::prelude::WebViewExt::find_controller(&view) {
                fc.search_previous();
            }
        });
    }
    {
        let view = web_view.clone();
        let whole_book_toggle = whole_book_toggle.clone();
        search_next.connect_clicked(move |_| {
            if whole_book_toggle.is_active() {
                return;
            }
            if let Some(fc) = webkit6::prelude::WebViewExt::find_controller(&view) {
                fc.search_next();
            }
        });
    }
    if let Some(fc) = webkit6::prelude::WebViewExt::find_controller(&web_view) {
        let count_label = search_count.clone();
        let toggle = whole_book_toggle.clone();
        fc.connect_found_text(move |_, count| {
            if !toggle.is_active() {
                count_label.set_text(&format!("{count} found"));
            }
        });
        let count_label = search_count.clone();
        let toggle = whole_book_toggle.clone();
        fc.connect_failed_to_find_text(move |_| {
            if !toggle.is_active() {
                count_label.set_text("No matches");
            }
        });
    }

    {
        let key_controller = gtk4::EventControllerKey::new();
        let epub_undo = epub_undo.clone();
        let epub_redo = epub_redo.clone();
        let search_toggle = search_toggle.clone();
        key_controller.connect_key_pressed(move |_, keyval, _keycode, modifiers| {
            if keyval == gdk::Key::z && modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                    epub_redo();
                } else {
                    epub_undo();
                }
                return glib::Propagation::Stop;
            }
            if keyval == gdk::Key::f && modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                search_toggle.set_active(!search_toggle.is_active());
                return glib::Propagation::Stop;
            }
            if keyval == gdk::Key::Escape && search_toggle.is_active() {
                search_toggle.set_active(false);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.add_controller(key_controller);
    }

    if let Some(sidebar_toggle) = &sidebar_toggle {
        let paned = paned.clone();
        let contents_scroll = contents_scroll.clone();
        sidebar_toggle.connect_toggled(move |btn| {
            if btn.is_active() {
                paned.set_start_child(contents_scroll.as_ref());
            } else {
                paned.set_start_child(gtk4::Widget::NONE);
            }
        });
    }
    {
        let notes_paned = notes_paned.clone();
        let notes_scroll = notes_scroll.clone();
        let rebuild_notes = rebuild_notes.clone();
        const NOTES_SIDEBAR_WIDTH: i32 = 300;
        notes_toggle.connect_toggled(move |btn| {
            if btn.is_active() {
                rebuild_notes();
                let available = notes_paned.width();
                let total = if available > 0 { available } else { 900 };
                notes_paned.set_position((total - NOTES_SIDEBAR_WIDTH).max(200));
                notes_paned.set_end_child(Some(&notes_scroll));
            } else {
                notes_paned.set_end_child(gtk4::Widget::NONE);
            }
        });
    }

    // Re-apply saved highlights after every chapter load (initial load, TOC jump,
    // prev/next, a notes-sidebar jump — all funnel through `epub_go_to`'s `load_uri`, so
    // one handler here covers all of them), consuming `pending_scroll` if the navigation
    // that triggered this load set one. Also consumes `pending_scroll_percent` (reading-
    // position resume, first load only) and `pending_search` (a whole-book search result's
    // chapter jump, handed to WebKit's own `FindController` once the page is actually there).
    {
        let reader = reader.clone();
        let pending_scroll = pending_scroll.clone();
        let pending_scroll_percent = pending_scroll_percent.clone();
        let pending_search = pending_search.clone();
        web_view.connect_load_changed(move |view, event| {
            if event == webkit6::LoadEvent::Finished {
                let scroll_to = pending_scroll.borrow_mut().take();
                epub_apply_highlights(view, &reader, scroll_to.as_deref());
                if let Some(percent) = pending_scroll_percent.borrow_mut().take() {
                    let script = format!(
                        "window.scrollTo(0, Math.round((document.documentElement.scrollHeight - document.documentElement.clientHeight) * {}));",
                        (percent.min(100) as f64) / 100.0
                    );
                    view.evaluate_javascript(&script, None, None, gio::Cancellable::NONE, |_| {});
                }
                if let Some(query) = pending_search.borrow_mut().take() {
                    if let Some(fc) = webkit6::prelude::WebViewExt::find_controller(view) {
                        let options = (webkit6::FindOptions::CASE_INSENSITIVE
                            | webkit6::FindOptions::WRAP_AROUND)
                            .bits();
                        fc.search(&query, options, 1000);
                    }
                }
            }
        });
    }

    // Load the first chapter up front (the TOC/prev/next/notes-jump handlers all reuse
    // this same navigation path for consistency, but chapter 0 has to start somewhere).
    let first_chapter = reader.borrow().spine.get(start_index).cloned();
    if let Some(first) = first_chapter {
        epub_go_to(&reader, &web_view, &prev, &next, &chapter_label, &first);
    }

    {
        let reader = reader.clone();
        let view = web_view.clone();
        let prev_for_handler = prev.clone();
        let next = next.clone();
        let chapter_label = chapter_label.clone();
        prev.connect_clicked(move |_| {
            let target = {
                let r = reader.borrow();
                (r.index > 0).then(|| r.spine[r.index - 1].clone())
            };
            if let Some(target) = target {
                epub_go_to(
                    &reader,
                    &view,
                    &prev_for_handler,
                    &next,
                    &chapter_label,
                    &target,
                );
            }
        });
    }
    {
        let reader = reader.clone();
        let view = web_view.clone();
        let prev = prev.clone();
        let next_for_handler = next.clone();
        let chapter_label = chapter_label.clone();
        next.connect_clicked(move |_| {
            let target = {
                let r = reader.borrow();
                (r.index + 1 < r.spine.len()).then(|| r.spine[r.index + 1].clone())
            };
            if let Some(target) = target {
                epub_go_to(
                    &reader,
                    &view,
                    &prev,
                    &next_for_handler,
                    &chapter_label,
                    &target,
                );
            }
        });
    }

    {
        let reader = reader.clone();
        let view = web_view.clone();
        let host = host.clone();
        let mode_drop = mode_drop.clone();
        let color_drop = color_drop.clone();
        let rebuild_notes = rebuild_notes.clone();
        apply_button.connect_clicked(move |_| {
            let reader = reader.clone();
            let view_for_apply = view.clone();
            let host = host.clone();
            let kind = EPUB_MARK_KIND_OPTIONS
                .get(mode_drop.selected() as usize)
                .map(|(_, k)| *k)
                .unwrap_or(fond_bib::AnnotationKind::Highlight);
            let color = COLOR_PRESETS
                .get(color_drop.selected() as usize)
                .map(|(_, hex)| hex.to_string());
            let rebuild_notes = rebuild_notes.clone();
            view.evaluate_javascript(
                EPUB_CAPTURE_SELECTION_JS,
                None,
                None,
                gio::Cancellable::NONE,
                move |result| {
                    let raw = match result {
                        Ok(v) => v.to_str().to_string(),
                        Err(e) => {
                            host.notify(&format!("Could not read selection: {e}"));
                            return;
                        }
                    };
                    let capture: EpubSelectionCapture = match serde_json::from_str(&raw) {
                        Ok(c) => c,
                        Err(_) => {
                            host.notify("Could not read selection");
                            return;
                        }
                    };
                    let snippet = capture.text.filter(|t| !t.trim().is_empty());
                    let Some(snippet) = (!capture.empty).then_some(snippet).flatten() else {
                        host.notify("Select some text first");
                        return;
                    };

                    let chapter = {
                        let r = reader.borrow();
                        r.spine.get(r.index).cloned()
                    };
                    let Some(chapter) = chapter else {
                        return;
                    };

                    let mut annotation = fond_bib::Annotation::drawn_epub(
                        kind,
                        chapter,
                        snippet,
                        capture.prefix,
                        capture.suffix,
                        None,
                    );
                    if kind == fond_bib::AnnotationKind::Highlight {
                        annotation.color = color.clone();
                    }
                    let id = annotation.id.clone();
                    push_epub_undo_snapshot(&reader);
                    reader.borrow_mut().annotations.upsert(annotation);

                    let write_result = host.save_annotations(&reader.borrow().annotations);
                    match write_result {
                        Ok(()) => {
                            epub_apply_highlights(&view_for_apply, &reader, Some(&id));
                            host.notify("Added");
                            rebuild_notes();
                        }
                        Err(e) => host.notify(&e),
                    }
                },
            );
        });
    }

    // Save the current chapter+scroll-percent back to the entry's Progress on close, so the
    // next "Read" resumes where this session left off — the EPUB half of the same resume
    // Tier 2a already gives the PDF reader (see its own `connect_close_request` above).
    // Reading `document.documentElement.scrollTop`/`scrollHeight`/`clientHeight` is async
    // (`evaluate_javascript`), so this returns `Propagation::Proceed` immediately and writes
    // the note in the callback — nothing after that write depends on the dialog still being
    // open, it just needs `state`/`key`, both cheap `Rc`/`String` clones.
    {
        let host = host.clone();
        let hash = hash.to_string();
        let reader = reader.clone();
        let view = web_view.clone();
        dialog.connect_close_request(move |_| {
            reader::unregister_window(&hash);
            let (chapter_num, chapter_count) = {
                let r = reader.borrow();
                (r.index as u32 + 1, r.spine.len() as u32)
            };
            let host = host.clone();
            let script = "(function() {\n  var el = document.documentElement;\n  var range = el.scrollHeight - el.clientHeight;\n  return range > 0 ? Math.round((el.scrollTop / range) * 100) : 0;\n})()";
            view.evaluate_javascript(script, None, None, gio::Cancellable::NONE, move |result| {
                let percent: u8 = result
                    .ok()
                    .map(|v| v.to_int32().clamp(0, 100) as u8)
                    .unwrap_or(0);
                host.save_progress(fond_bib::Progress {
                    page: chapter_num,
                    of: chapter_count,
                    chapter_percent: Some(percent),
                });
            });
            glib::Propagation::Proceed
        });
    }

    dialog.present();
}

/// Navigate the EPUB reader's `WebView` to `target` (a zip-internal path, optionally with a
/// `#fragment` for an in-chapter anchor — the same shape `fond_doc::EpubBook::spine`/`toc`
/// entries use). Updates `state.index` when `target`'s path (fragment stripped) matches a
/// spine entry, so the chapter label and prev/next sensitivity stay correct whether the jump
/// came from a TOC entry, the prev/next buttons, or the initial chapter-0 load — all three
/// funnel through here rather than duplicating the URI-building and label/button refresh.
fn epub_go_to(
    state: &Rc<RefCell<EpubReaderState>>,
    view: &webkit6::WebView,
    prev: &gtk4::Button,
    next: &gtk4::Button,
    chapter_label: &gtk4::Label,
    target: &str,
) {
    let (path, fragment) = target
        .split_once('#')
        .map_or((target, None), |(p, f)| (p, Some(f)));

    let cache_dir = {
        let mut r = state.borrow_mut();
        if let Some(idx) = r.spine.iter().position(|p| p == path) {
            r.index = idx;
        }
        r.cache_dir.clone()
    };

    let mut uri = gio::File::for_path(cache_dir.join(path)).uri().to_string();
    if let Some(fragment) = fragment {
        uri.push('#');
        uri.push_str(fragment);
    }
    view.load_uri(&uri);

    let r = state.borrow();
    chapter_label.set_text(&format!("Chapter {} of {}", r.index + 1, r.spine.len()));
    prev.set_sensitive(r.index > 0);
    next.set_sensitive(r.index + 1 < r.spine.len());
}
