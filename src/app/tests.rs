use super::{
    SessionNavigation, StableListScrollbarHandle, StreamDeltaKind, TranscriptRowKind::*,
    append_text_delta_to_session, apply_transcript_visibility_splice, escape_html,
    estimated_message_height, estimated_text_height, fenced_code, folded_transcript_row_kinds,
    format_worked_duration, maintain_transcript_anchor, markdown_estimation_source,
    message_starts_followup_turn, pop_stream_chunk, prepare_transcript_row_remeasurement,
    scale_scrollbar_offset, scroll_top_after_row_invalidation,
    stabilized_transcript_anchor_end_space, take_stream_prefix, transcript_anchor_end_space,
    transcript_row_kinds, transcript_row_splice,
};
use crate::model::{
    ActivityItem, ActivityKind, AgentSession, DriverEvent, Message, MessageRole, ProviderKind,
    ReasoningBlock, SessionStatus, TranscriptBlock, TranscriptBlockContent, TurnStatus,
};
use gpui::{ListAlignment, ListState, px};
use std::{
    cell::{Cell, RefCell},
    collections::{HashSet, VecDeque},
    rc::Rc,
};
use uuid::Uuid;

#[test]
fn session_navigation_tracks_back_forward_and_new_branches() {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let third = Uuid::new_v4();
    let branch = Uuid::new_v4();
    let mut navigation = SessionNavigation::default();

    navigation.visit(Some(first), second);
    navigation.visit(Some(second), third);
    assert_eq!(navigation.go_back(third), Some(second));
    assert_eq!(navigation.go_back(second), Some(first));
    assert_eq!(navigation.go_forward(first), Some(second));

    navigation.visit(Some(second), branch);
    assert_eq!(navigation.go_forward(branch), None);
    assert_eq!(navigation.go_back(branch), Some(second));
}

#[test]
fn session_navigation_prunes_deleted_tasks() {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let third = Uuid::new_v4();
    let mut navigation = SessionNavigation::default();

    navigation.visit(Some(first), second);
    navigation.visit(Some(second), third);
    assert_eq!(navigation.go_back(third), Some(second));

    navigation.remove(first);
    navigation.remove(third);
    assert_eq!(navigation.go_back(second), None);
    assert_eq!(navigation.go_forward(second), None);
}

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

    let underfilled = gpui::ListOffset {
        item_ix: 0,
        offset_in_item: gpui::px(-140.0),
    };
    let anchored = scroll_top_after_row_invalidation(underfilled, 0..1, gpui::Pixels::ZERO)
        .expect("the disclosure row contains the synthetic leading-space anchor");
    assert_eq!(anchored.offset_in_item, gpui::px(-140.0));
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
fn message_estimates_reserve_shared_footer() {
    let user = Message::new(MessageRole::User, "A short prompt.");
    let without_actions = estimated_message_height(&user, gpui::px(720.0), false);
    let with_actions = estimated_message_height(&user, gpui::px(720.0), true);

    assert_eq!(with_actions - without_actions, gpui::px(30.0));

    let assistant = Message::new(MessageRole::Assistant, "A short response.");
    let without_actions = estimated_message_height(&assistant, gpui::px(720.0), false);
    let with_actions = estimated_message_height(&assistant, gpui::px(720.0), true);

    assert_eq!(with_actions - without_actions, gpui::px(30.0));
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

#[test]
fn settled_turn_folds_interim_text_and_work_but_keeps_the_final_response() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id, ProviderKind::Codex);
    let turn_id = session.begin_turn("Build it");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 1,
        turn_id: Some(turn_id),
        content: TranscriptBlockContent::Reasoning(ReasoningBlock {
            content: "Looking around".into(),
            started_at_ms: 1_000,
            finished_at_ms: 2_000,
        }),
    });
    session.push_message(MessageRole::Assistant, "I found the relevant code.");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 2,
        turn_id: Some(turn_id),
        content: TranscriptBlockContent::Activities(vec![ActivityItem::new(
            None,
            ActivityKind::Command,
            "Ran tests",
            None,
            true,
        )]),
    });
    session.push_message(MessageRole::Assistant, "Done. The change is ready.");
    session.finish_active_turn(TurnStatus::Completed);

    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), TurnFold(turn_id), Message(2)]
    );
    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::from([turn_id])),
        vec![
            Message(0),
            TurnFold(turn_id),
            TurnBlock(0),
            Message(1),
            TurnBlock(1),
            Message(2)
        ]
    );
}

#[test]
fn turn_fold_visibility_splice_preserves_surrounding_message_rows() {
    let turn_id = Uuid::new_v4();
    let collapsed = vec![Message(0), TurnFold(turn_id), Message(2)];
    let expanded = vec![
        Message(0),
        TurnFold(turn_id),
        TurnBlock(0),
        Message(1),
        TurnBlock(1),
        Message(2),
    ];

    let expand_splice = transcript_row_splice(&collapsed, &expanded);
    assert_eq!(expand_splice, Some((2..2, 3)));
    assert_eq!(
        transcript_row_splice(&expanded, &collapsed),
        Some((2..5, 0))
    );
    assert_eq!(transcript_row_splice(&collapsed, &collapsed), None);

    let transcript_rows = ListState::new(collapsed.len(), ListAlignment::Bottom, px(0.0));
    let anchored_rows = ListState::new(collapsed.len(), ListAlignment::Top, px(0.0));
    let provisional_rows = RefCell::new(HashSet::from([0, 1, 2]));
    let exact_measurement_rows = RefCell::new(HashSet::from([1]));

    apply_transcript_visibility_splice(
        [&transcript_rows, &anchored_rows],
        collapsed.len(),
        expanded.len(),
        expand_splice,
        &provisional_rows,
        &exact_measurement_rows,
    );

    assert_eq!(transcript_rows.item_count(), expanded.len());
    assert_eq!(anchored_rows.item_count(), expanded.len());
    assert!(provisional_rows.borrow().is_empty());
    assert_eq!(*exact_measurement_rows.borrow(), HashSet::from([2, 3, 4]));
}

#[test]
fn local_message_remeasurement_never_queues_blank_placeholder_rows() {
    let provisional_rows = RefCell::new(HashSet::from([1, 4]));
    let exact_measurement_rows = RefCell::new(HashSet::from([1, 2, 4]));

    prepare_transcript_row_remeasurement(&provisional_rows, &exact_measurement_rows, 1..3, false);

    assert_eq!(*provisional_rows.borrow(), HashSet::from([4]));
    assert_eq!(*exact_measurement_rows.borrow(), HashSet::from([1, 2, 4]));
}

#[test]
fn bulk_transcript_reflow_can_explicitly_queue_placeholder_rows() {
    let provisional_rows = RefCell::new(HashSet::new());
    let exact_measurement_rows = RefCell::new(HashSet::from([2]));

    prepare_transcript_row_remeasurement(&provisional_rows, &exact_measurement_rows, 1..4, true);

    assert_eq!(*provisional_rows.borrow(), HashSet::from([1, 2, 3]));
    assert!(exact_measurement_rows.borrow().is_empty());
}

#[test]
fn running_turn_keeps_its_ordered_work_visible() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id, ProviderKind::Codex);
    let turn_id = session.begin_turn("Keep going");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 1,
        turn_id: Some(turn_id),
        content: TranscriptBlockContent::Reasoning(ReasoningBlock {
            content: "Still thinking".into(),
            started_at_ms: 1_000,
            finished_at_ms: 2_000,
        }),
    });
    session.push_message(MessageRole::Assistant, "Interim update");

    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), TurnBlock(0), Message(1)]
    );
}

#[test]
fn plain_settled_response_does_not_add_an_empty_work_fold() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id, ProviderKind::Codex);
    session.begin_turn("Answer directly");
    session.push_message(MessageRole::Assistant, "The answer.");
    session.finish_active_turn(TurnStatus::Completed);

    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), Message(1)]
    );
}

#[test]
fn worked_duration_uses_readable_units() {
    assert_eq!(format_worked_duration(1), "1 second");
    assert_eq!(format_worked_duration(28), "28 seconds");
    assert_eq!(format_worked_duration(60), "1 minute");
    assert_eq!(format_worked_duration(88), "1 minute 28 seconds");
    assert_eq!(format_worked_duration(7_320), "2 hours 2 minutes");
}
