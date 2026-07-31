use super::{
    StableListScrollbarHandle, StreamDeltaKind, TranscriptRowKind::*, append_text_delta_to_session,
    escape_html, estimated_message_height, estimated_text_height, fenced_code,
    maintain_transcript_anchor, markdown_estimation_source, message_starts_followup_turn,
    pop_stream_chunk, scale_scrollbar_offset, scroll_top_after_row_invalidation,
    stabilized_transcript_anchor_end_space, take_stream_prefix, transcript_anchor_end_space,
    transcript_row_kinds,
};
use crate::model::{
    ActivityKind, AgentSession, DriverEvent, Message, MessageRole, ProviderKind, SessionStatus,
};
use std::{cell::Cell, collections::VecDeque, rc::Rc};

#[test]
fn stable_scrollbar_maps_live_offsets_without_changing_progress() {
    let actual_max = gpui::size(gpui::px(0.0), gpui::px(600.0));
    let stable_max = gpui::size(gpui::px(0.0), gpui::px(2_400.0));

    assert_eq!(
        scale_scrollbar_offset(
            gpui::point(gpui::px(0.0), gpui::px(-300.0)),
            actual_max,
            stable_max,
        ),
        gpui::point(gpui::px(0.0), gpui::px(-1_200.0))
    );
    assert_eq!(
        scale_scrollbar_offset(
            gpui::point(gpui::px(0.0), gpui::px(-2_400.0)),
            stable_max,
            actual_max,
        ),
        gpui::point(gpui::px(0.0), gpui::px(-600.0))
    );
}

#[test]
fn stable_scrollbar_freezes_its_document_height_during_a_drag() {
    use gpui_component::scroll::ScrollbarHandle as _;

    let rows = gpui::ListState::new(0, gpui::ListAlignment::Bottom, gpui::px(0.0));
    let estimated = Rc::new(Cell::new(gpui::px(1_000.0)));
    let anchor_end_space = Rc::new(Cell::new(gpui::px(300.0)));
    let anchor_following = Rc::new(Cell::new(true));
    let drag_estimate = Rc::new(Cell::new(None));
    let is_scrolled = Rc::new(Cell::new(false));
    let handle = StableListScrollbarHandle::new(
        &rows,
        &estimated,
        &anchor_end_space,
        &anchor_following,
        &drag_estimate,
        &is_scrolled,
    );

    handle.start_drag();
    estimated.set(gpui::px(2_000.0));
    anchor_end_space.set(gpui::px(0.0));
    assert_eq!(handle.content_size().height, gpui::px(1_300.0));
    handle.end_drag();
    assert_eq!(handle.content_size().height, gpui::px(2_000.0));
}

#[test]
fn anchor_end_space_keeps_a_short_new_turn_at_the_viewport_top() {
    assert_eq!(
        transcript_anchor_end_space(gpui::px(700.0), gpui::px(180.0)),
        gpui::px(520.0)
    );
    assert_eq!(
        transcript_anchor_end_space(gpui::px(700.0), gpui::px(900.0)),
        gpui::px(0.0)
    );
}

#[test]
fn anchor_end_space_waits_for_exact_expanded_row_measurement() {
    assert_eq!(
        stabilized_transcript_anchor_end_space(
            gpui::px(700.0),
            gpui::px(260.0),
            gpui::px(520.0),
            true,
        ),
        gpui::px(520.0)
    );
    assert_eq!(
        stabilized_transcript_anchor_end_space(
            gpui::px(700.0),
            gpui::px(260.0),
            gpui::px(520.0),
            false,
        ),
        gpui::px(440.0)
    );
    assert_eq!(
        stabilized_transcript_anchor_end_space(
            gpui::px(700.0),
            gpui::px(180.0),
            gpui::px(440.0),
            true,
        ),
        gpui::px(520.0)
    );
}

#[test]
fn pending_expansion_reasserts_the_user_message_anchor() {
    let rows = gpui::ListState::new(3, gpui::ListAlignment::Bottom, gpui::px(0.0));
    rows.scroll_to(gpui::ListOffset {
        item_ix: 0,
        offset_in_item: gpui::px(42.0),
    });

    assert!(maintain_transcript_anchor(&rows, 0, true, gpui::px(320.0),));
    let anchored = rows.logical_scroll_top();
    assert_eq!(anchored.item_ix, 0);
    assert_eq!(anchored.offset_in_item, gpui::Pixels::ZERO);
    assert!(!maintain_transcript_anchor(
        &rows,
        0,
        true,
        gpui::Pixels::ZERO,
    ));
}

#[test]
fn row_invalidation_preserves_the_intra_message_anchor() {
    let scroll_top = gpui::ListOffset {
        item_ix: 4,
        offset_in_item: gpui::px(320.0),
    };
    let anchored = scroll_top_after_row_invalidation(scroll_top, 4..5, gpui::px(80.0))
        .expect("the invalidated row contains the scroll top");
    assert_eq!(anchored.item_ix, 4);
    assert_eq!(anchored.offset_in_item, gpui::px(400.0));
    assert!(scroll_top_after_row_invalidation(scroll_top, 5..6, gpui::px(80.0)).is_none());
}

#[test]
fn transcript_estimates_count_explicit_markdown_lines() {
    let markdown = (1..=200)
        .map(|line| format!("{line}. A short sentence."))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(estimated_text_height(&markdown, 88, 21.0) >= gpui::px(4_200.0));
}

#[test]
fn markdown_estimates_reserve_images_without_counting_long_sources_as_text() {
    let markdown = "Before\n\n![preview](data:image/png;base64,AAAAAAAAAAAAAAAAAAAAAAAA)\n\nAfter";
    let (visible, media_height) = markdown_estimation_source(markdown);
    assert!(!visible.contains("base64"));
    assert_eq!(media_height, gpui::px(160.0));

    let message = Message::new(MessageRole::Assistant, markdown);
    assert!(estimated_message_height(&message, gpui::px(720.0), false) >= gpui::px(200.0));
}

#[test]
fn assistant_estimate_reserves_only_a_visible_checkpoint() {
    let message = Message::new(MessageRole::Assistant, "A short response.");
    let without_checkpoint = estimated_message_height(&message, gpui::px(720.0), false);
    let with_checkpoint = estimated_message_height(&message, gpui::px(720.0), true);

    assert_eq!(with_checkpoint - without_checkpoint, gpui::px(28.0));
}

#[test]
fn markdown_estimates_only_visible_disclosure_content() {
    let closed = "<details><summary>More</summary>hidden ![image](x.png)</details>";
    let (visible, media_height) = markdown_estimation_source(closed);
    assert!(visible.contains("More"));
    assert!(!visible.contains("hidden"));
    assert_eq!(media_height, gpui::Pixels::ZERO);

    let open = "<details open><summary>More</summary>visible ![image](x.png)</details>";
    let (visible, media_height) = markdown_estimation_source(open);
    assert!(visible.contains("visible"));
    assert_eq!(media_height, gpui::px(160.0));

    let nested = "<DETAILS OPEN><SUMMARY>Outer</SUMMARY><details><summary>Inner</summary>hidden</details><IMG HEIGHT='245' src='x'></DETAILS>";
    let (visible, media_height) = markdown_estimation_source(nested);
    assert!(visible.contains("Outer"));
    assert!(visible.contains("Inner"));
    assert!(!visible.contains("hidden"));
    assert_eq!(media_height, gpui::px(245.0));

    let (visible, media_height) =
        markdown_estimation_source("<details-panel>ordinary text</details-panel>");
    assert!(visible.contains("ordinary text"));
    assert_eq!(media_height, gpui::Pixels::ZERO);
}

#[test]
fn plain_message_html_is_escaped() {
    assert_eq!(
        escape_html("<tag a='b'>&\""),
        "&lt;tag a=&#39;b&#39;&gt;&amp;&quot;"
    );
}

#[test]
fn only_later_user_messages_start_followup_turns() {
    let messages = vec![
        Message::new(MessageRole::User, "first"),
        Message::new(MessageRole::Assistant, "answer"),
        Message::new(MessageRole::User, "follow-up"),
        Message::new(MessageRole::Assistant, "answer"),
    ];
    assert!(!message_starts_followup_turn(&messages, 0));
    assert!(!message_starts_followup_turn(&messages, 1));
    assert!(message_starts_followup_turn(&messages, 2));
    assert!(!message_starts_followup_turn(&messages, 3));
}

#[test]
fn fenced_code_collects_all_blocks_without_languages() {
    let markdown = "Before\n```rust\nfn main() {}\n```\nAfter\n```\ncargo test\n```";
    assert_eq!(
        fenced_code(markdown).as_deref(),
        Some("fn main() {}\n\ncargo test")
    );
    assert_eq!(fenced_code("No code here"), None);
}

#[test]
fn stream_prefix_stops_at_lines_without_splitting_graphemes() {
    let mut text = "hello 👋🏽\nnext line".to_owned();
    let (first, count) = take_stream_prefix(&mut text, 100);
    assert_eq!(first, "hello 👋🏽\n");
    assert_eq!(count, 8);
    assert_eq!(text, "next line");

    let mut emoji = "👨‍👩‍👧‍👦x".to_owned();
    let (first, count) = take_stream_prefix(&mut emoji, 1);
    assert_eq!(first, "👨‍👩‍👧‍👦");
    assert_eq!(count, 1);
    assert_eq!(emoji, "x");
}

#[test]
fn stream_chunks_coalesce_deltas_and_preserve_event_order() {
    let mut events = VecDeque::from([
        DriverEvent::TextDelta("first ".into()),
        DriverEvent::TextDelta("line\nsecond line".into()),
        DriverEvent::Activity {
            id: None,
            kind: ActivityKind::Tool,
            title: "Tool".into(),
            detail: None,
            complete: true,
        },
        DriverEvent::TextDelta("after tool".into()),
    ]);

    assert!(matches!(
        pop_stream_chunk(&mut events, StreamDeltaKind::Text),
        Some(DriverEvent::TextDelta(text)) if text == "first line\n"
    ));
    assert!(matches!(
        events.front(),
        Some(DriverEvent::TextDelta(text)) if text == "second line"
    ));

    assert!(matches!(
        pop_stream_chunk(&mut events, StreamDeltaKind::Text),
        Some(DriverEvent::TextDelta(text)) if text == "second line"
    ));
    assert!(matches!(events.front(), Some(DriverEvent::Activity { .. })));
}

#[test]
fn stream_parts_keep_targeting_the_running_session_after_selection_changes() {
    let project_id = uuid::Uuid::new_v4();
    let mut running = AgentSession::new(project_id, ProviderKind::Codex);
    running.begin_turn("background task");
    running.status = SessionStatus::Working;
    let running_id = running.id;
    let visible = AgentSession::new(project_id, ProviderKind::Claude);
    let visible_id = visible.id;
    let mut sessions = vec![running, visible];

    append_text_delta_to_session(&mut sessions, running_id, false, "first".into());
    // Navigation changes only which task is rendered. The runtime keeps
    // emitting with its own task ID while another task is visible.
    let selected_session = visible_id;
    append_text_delta_to_session(&mut sessions, running_id, true, " second".into());

    assert_eq!(selected_session, visible_id);
    assert_eq!(sessions[0].messages[1].content, "first second");
    assert!(sessions[0].messages[1].streaming);
    assert!(sessions[1].messages.is_empty());
}

#[test]
fn turn_blocks_keep_their_message_boundaries() {
    // user, assistant text, tool row, assistant text, reasoning row,
    // assistant text
    let rows = transcript_row_kinds(4, &[2, 3]);
    assert_eq!(
        rows,
        vec![
            Message(0),
            Message(1),
            TurnBlock(0),
            Message(2),
            TurnBlock(1),
            Message(3)
        ]
    );
}

#[test]
fn blocks_follow_the_latest_message_without_a_reply() {
    let rows = transcript_row_kinds(2, &[2]);
    assert_eq!(rows, vec![Message(0), Message(1), TurnBlock(0)]);
}

#[test]
fn plain_transcript_maps_one_to_one() {
    let rows = transcript_row_kinds(4, &[]);
    assert_eq!(rows, vec![Message(0), Message(1), Message(2), Message(3)]);
}

#[test]
fn multiple_blocks_at_one_boundary_preserve_event_order() {
    let rows = transcript_row_kinds(2, &[1, 1]);
    assert_eq!(
        rows,
        vec![Message(0), TurnBlock(0), TurnBlock(1), Message(1)]
    );
}
