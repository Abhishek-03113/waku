# Waku patch

This is `gpui-component` 0.5.1 from crates.io, vendored because its
non-scrollable `TextView` root renders every top-level Markdown node with
`is_last = true`. That suppresses `TextViewStyle::paragraph_gap` for the
entire document.

Waku patches `src/text/node.rs` so root children receive `is_last = true`
only for the actual final child, and applies the same computed block margin
to headings and tables. This preserves one selectable `TextView` while
restoring the configured spacing between Markdown blocks.

It also adds `ContextMenuExt::context_menu_with_id` so repeated context
menus, such as sidebar session rows, can keep independent GPUI element state.

`TextView::update_delay` makes the source-update debounce configurable. Waku
uses a short delay for live Markdown so streamed messages can keep one stable
keyed view instead of creating new GPUI state for every chunk. Source updates
are throttled rather than repeatedly postponing the parse, so a busy stream
keeps making visible progress at a bounded render rate.
Repeated construction of an unchanged keyed view also no longer enqueues a
duplicate source update on every scroll frame.

Selectable text now follows Zed's document-level interaction model: one hitbox
on the `TextView`, while inline hitboxes and mouse handlers are created only for
actual links. Plain long responses no longer register interaction work per line.
`TextView::selection_scroll_handle` also keeps the drag endpoint in window
coordinates and scrolls the owning transcript list near its viewport edges, so
selection can continue beyond the currently visible portion of a message. Waku
owns each message's `TextViewState` so a virtualized row remount cannot discard
the range, and identical style updates no longer enqueue redundant reparses.
The text hitbox and selection start area are clipped to the transcript viewport,
so an offscreen portion of a tall message cannot receive clicks behind the app
header while an active drag can still leave the viewport to autoscroll.
Secondary mouse input never starts, ends, or clears a text selection, and the
custom context-menu trigger consumes the event after opening its menu. This
keeps the selected range visibly highlighted while the menu is open.
The list scrollbar offset is clamped to its measured maximum when a bottom-
aligned transcript snaps to its pinned-tail state. This keeps the thumb valid
and visible through the normal idle delay at the end of the transcript.
Waku also seeds the outer transcript list with lightweight per-row height
estimates and presents their total through a normalized scrollbar handle.
Exact row measurements can replace those estimates as messages enter the
viewport without resizing the thumb or parsing every offscreen message first.

Long, simple Markdown lists are rendered as small multiline text chunks inside
the same `TextView`. This removes hundreds of flex/layout subtrees without
fragmenting the message or its selection model.

Long heterogeneous documents use measured top-level block virtualization.
`TextViewScrollViewport` captures the owning transcript viewport before the
outer GPUI list begins row layout, avoiding a nested `ListState` borrow. The
first layout uses estimated block heights and immediately builds only a bounded
window; once the row origin is known, a corrective frame targets the actual
visible window plus one viewport of overscan. There is no full-document warm-up
layout. Streaming updates retain measurements for
the unchanged AST prefix and invalidate only the changed tail, including code
content changes. During a drag selection the complete document is rendered so
selection and copy remain continuous across Markdown node types, then block
virtualization resumes immediately after the endpoint settles.

Measured blocks now report post-layout height changes to their virtualized
parent. Initial discovery remains silent, while real changes such as an image
finishing loading, an animated or custom element changing size, or a disclosure
opening invalidate exactly one transcript row. Waku applies the measured delta
to its stable scrollbar estimate, preserves the intra-message pixel anchor when
the resized block is above the viewport, coalesces repeated changes, and freezes
both the real and estimated document lengths during scrollbar drags. Reparsed
tails retract previously reported resize adjustments so stale media dimensions
cannot leak into a later document.

Markdown and HTML images reserve a bounded placeholder, honor both explicit
width and height, cap pathological heights, and keep a same-height loading or
error fallback. Offscreen images remain unloaded. HTML `<details>` and
`<summary>` are interactive, keyboard-focusable, retain their open state while
their streamed body changes, and flow through the same resize notification
path. Width changes invalidate wrap-dependent block measurements before
visible-range selection and rebase prior media/disclosure height corrections,
while the outer transcript bulk-measures only lightweight row placeholders so
resizing a long session does not parse every offscreen message. Appending a row
updates only the previous tail and new estimates rather than rescanning old
Markdown, and image estimation avoids duplicating large data URLs.

Markdown table cells use normal line wrapping instead of truncating their text.
This keeps long prose readable within the allocated column width rather than
silently clipping it behind the table border.
