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
The list scrollbar offset is clamped to its measured maximum when a bottom-
aligned transcript snaps to its pinned-tail state. This keeps the thumb valid
and visible through the normal idle delay at the end of the transcript.

Long, simple Markdown lists are rendered as small multiline text chunks inside
the same `TextView`. This removes hundreds of flex/layout subtrees without
fragmenting the message or its selection model.
