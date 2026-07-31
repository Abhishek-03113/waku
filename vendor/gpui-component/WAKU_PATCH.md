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
keyed view instead of creating new GPUI state for every chunk.
