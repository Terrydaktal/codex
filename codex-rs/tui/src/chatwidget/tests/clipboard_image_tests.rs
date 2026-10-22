//! Clipboard snapshots stay inline through editing, queueing, and submission.

use super::*;
use crate::clipboard_paste::EncodedImageFormat;
use crate::clipboard_paste::MAX_CLIPBOARD_IMAGE_DATA_URL_BYTES;
use base64::Engine;
use codex_app_server_protocol::ImageReference;
use pretty_assertions::assert_eq;

fn clipboard_image() -> Result<(String, PastedImageInfo), PasteImageError> {
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(
        /*width*/ 2,
        /*height*/ 1,
        image::Rgba([10, 20, 30, 255]),
    )
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    Ok((
        format!("data:image/png;base64,{encoded}"),
        PastedImageInfo {
            width: 2,
            height: 1,
            encoded_format: EncodedImageFormat::Png,
        },
    ))
}

fn clipboard_inputs() -> Vec<UserInput> {
    vec![
        UserInput::Image {
            image: ImageReference::Inline {
                url: clipboard_image().unwrap().0,
            },
            detail: None,
        },
        UserInput::Text {
            text: "[Image #1]".into(),
            text_elements: app_server_text_elements(&[TextElement::new(
                (0..10).into(),
                Some("[Image #1]".into()),
            )]),
        },
    ]
}

#[tokio::test]
async fn terminal_paste_prefers_a_clipboard_image_over_its_text_payload() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.clipboard_image_paster = clipboard_image;
    chat.insert_str("explain this image");

    chat.handle_paste("file:///tmp/Spectacle.already-deleted/image.png".into());

    assert_eq!(
        chat.bottom_pane.composer_text(),
        "explain this image[Image #1]"
    );
    assert_eq!(chat.remote_image_urls(), vec![clipboard_image().unwrap().0]);
    assert!(chat.bottom_pane.composer_local_images().is_empty());
    assert_chatwidget_snapshot!(
        "clipboard_image_inline_draft",
        normalize_snapshot_paths(render_bottom_popup(&chat, /*width*/ 80))
    );
}

#[tokio::test]
async fn clipboard_image_shortcuts_submit_captured_bytes_without_rereading_clipboard() {
    for modifiers in [
        KeyModifiers::CONTROL,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ] {
        let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
        chat.thread_id = Some(ThreadId::new());
        chat.clipboard_image_paster = clipboard_image;
        assert!(matches!(
            chat.handle_key_event(KeyEvent::new(KeyCode::Char('v'), modifiers)),
            KeyEventAction::PasteImage
        ));
        chat.paste_image();
        chat.clipboard_image_paster = || panic!("submission must not reread the clipboard");

        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

        let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
            panic!("expected an image-only turn");
        };
        assert_eq!(items, clipboard_inputs());
        assert!(chat.remote_image_urls().is_empty());
        assert!(op_rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn clipboard_image_queued_before_session_keeps_its_snapshot() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_queue_submissions_until_session_configured(/*queue*/ true);
    chat.clipboard_image_paster = clipboard_image;
    chat.paste_image();
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_no_submit_op(&mut op_rx);
    assert_eq!(
        chat.input_queue
            .queued_user_messages
            .front()
            .unwrap()
            .remote_image_urls,
        vec![clipboard_image().unwrap().0]
    );
    chat.clipboard_image_paster = || panic!("queue draining must not reread the clipboard");

    chat.thread_id = Some(ThreadId::new());
    chat.maybe_send_next_queued_input();

    let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
        panic!("expected the queued image");
    };
    assert_eq!(items, clipboard_inputs());
}

#[tokio::test]
async fn clipboard_image_survives_startup_draft_handoff() {
    let (mut source, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    source.clipboard_image_paster = clipboard_image;
    source.paste_image();
    source.insert_str("keep this draft");
    let snapshot = source.bottom_pane.composer_draft_snapshot();
    let mut draft = Some(snapshot.clone());
    let mut confirmed = false;
    drop(source);
    let (mut restored, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    restored.restore_startup_input_when_ready(&mut draft, &mut confirmed);

    assert!(draft.is_none());
    assert_eq!(restored.bottom_pane.composer_draft_snapshot(), snapshot);
}

#[tokio::test]
async fn clipboard_image_inline_placeholders_relabel_local_images_and_can_be_removed() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.clipboard_image_paster = clipboard_image;
    chat.bottom_pane
        .attach_image(test_path_buf("/tmp/local-image.png"));
    chat.paste_image();
    assert_eq!(chat.bottom_pane.composer_text(), "[Image #2][Image #1]");
    assert_chatwidget_snapshot!(
        "clipboard_image_selected_with_local_image",
        normalize_snapshot_paths(render_bottom_popup(&chat, /*width*/ 80))
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Backspace));

    assert!(chat.remote_image_urls().is_empty());
    assert_eq!(chat.bottom_pane.composer_text(), "[Image #1]");
    assert_eq!(
        chat.bottom_pane.composer_local_images(),
        vec![LocalImageAttachment {
            placeholder: "[Image #1]".into(),
            path: test_path_buf("/tmp/local-image.png"),
        }]
    );
}

#[tokio::test]
async fn clipboard_images_insert_at_cursor_and_delete_only_the_matching_bytes() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.clipboard_image_paster = clipboard_image;
    chat.insert_str("before after");
    chat.bottom_pane.set_composer_cursor(/*cursor*/ 7);
    chat.paste_image();
    chat.paste_image();
    assert_eq!(
        chat.bottom_pane.composer_text(),
        "before [Image #1][Image #2]after"
    );
    assert_eq!(
        chat.remote_image_urls(),
        vec![clipboard_image().unwrap().0; 2]
    );
    assert_chatwidget_snapshot!(
        "clipboard_images_at_cursor",
        render_bottom_popup(&chat, /*width*/ 80)
    );

    chat.bottom_pane.set_composer_cursor(/*cursor*/ 17);
    chat.handle_key_event(KeyEvent::from(KeyCode::Backspace));
    assert_eq!(chat.bottom_pane.composer_text(), "before [Image #1]after");
    assert_eq!(chat.remote_image_urls(), vec![clipboard_image().unwrap().0]);
    chat.handle_key_event(KeyEvent::from(KeyCode::Delete));
    assert_eq!(chat.bottom_pane.composer_text(), "before after");
    assert!(chat.remote_image_urls().is_empty());
}

#[tokio::test]
async fn clipboard_image_relabels_multiple_dropped_images_without_label_collisions() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.clipboard_image_paster = clipboard_image;
    for index in 1..=3 {
        chat.bottom_pane
            .attach_image(test_path_buf(&format!("/tmp/image-{index}.png")));
    }
    chat.paste_image();
    assert_eq!(
        chat.bottom_pane.composer_text(),
        "[Image #2][Image #3][Image #4][Image #1]"
    );
    chat.handle_key_event(KeyEvent::from(KeyCode::Backspace));
    assert_eq!(
        chat.bottom_pane.composer_text(),
        "[Image #1][Image #2][Image #3]"
    );
    assert_eq!(
        chat.bottom_pane.composer_local_images(),
        (1..=3)
            .map(|index| LocalImageAttachment {
                placeholder: format!("[Image #{index}]"),
                path: test_path_buf(&format!("/tmp/image-{index}.png")),
            })
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn clipboard_image_external_edit_preserves_atomic_placeholders_and_removes_deleted_images() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.clipboard_image_paster = clipboard_image;
    chat.paste_image();
    chat.paste_image();
    chat.apply_external_edit("only [Image #2] remains".into());
    assert_eq!(chat.bottom_pane.composer_text(), "only [Image #1] remains");
    assert_eq!(chat.remote_image_urls(), vec![clipboard_image().unwrap().0]);
    chat.bottom_pane.set_composer_cursor(/*cursor*/ 5);
    chat.handle_key_event(KeyEvent::from(KeyCode::Delete));
    assert_eq!(chat.bottom_pane.composer_text(), "only  remains");
    assert!(chat.remote_image_urls().is_empty());
}

#[tokio::test]
async fn clipboard_image_merged_queued_drafts_keep_distinct_inline_labels() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.clipboard_image_paster = clipboard_image;
    chat.paste_image();
    let draft = chat.bottom_pane.composer_draft_snapshot();
    let message = UserMessage {
        text: draft.text,
        text_elements: draft.text_elements,
        remote_image_urls: draft.remote_image_urls,
        local_images: Vec::new(),
        mention_bindings: Vec::new(),
    };
    let combined = merge_user_messages(vec![message.clone(), message]);
    assert_eq!(combined.text, "[Image #1]\n[Image #2]");
    assert_eq!(
        combined.remote_image_urls,
        vec![clipboard_image().unwrap().0; 2]
    );
    assert_eq!(
        combined.text_elements,
        vec![
            TextElement::new((0..10).into(), Some("[Image #1]".into())),
            TextElement::new((11..21).into(), Some("[Image #2]".into())),
        ]
    );
}

#[tokio::test]
async fn clipboard_image_does_not_become_a_selectable_remote_row() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.clipboard_image_paster = clipboard_image;
    chat.bottom_pane
        .set_remote_image_urls(vec!["https://example.com/old.png".into()]);
    chat.paste_image();
    assert_eq!(chat.bottom_pane.composer_text(), "[Image #2]");
    chat.bottom_pane.set_composer_cursor(/*cursor*/ 0);
    chat.handle_key_event(KeyEvent::from(KeyCode::Up));
    chat.handle_key_event(KeyEvent::from(KeyCode::Delete));
    assert_eq!(chat.remote_image_urls(), vec![clipboard_image().unwrap().0]);
    assert_eq!(chat.bottom_pane.composer_text(), "[Image #1]");
    chat.handle_key_event(KeyEvent::from(KeyCode::Delete));
    assert!(chat.remote_image_urls().is_empty());
    assert_eq!(chat.bottom_pane.composer_text(), "");
}

#[tokio::test]
async fn terminal_paste_keeps_text_when_clipboard_has_no_image() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.handle_paste("plain clipboard text".into());

    assert_eq!(chat.bottom_pane.composer_text(), "plain clipboard text");
    assert!(chat.remote_image_urls().is_empty());
}

#[tokio::test]
async fn oversized_clipboard_image_does_not_replace_the_draft_or_paste_a_path() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.insert_str("keep this draft");
    chat.clipboard_image_paster = || {
        let (_, info) = clipboard_image()?;
        Ok(("x".repeat(MAX_CLIPBOARD_IMAGE_DATA_URL_BYTES + 1), info))
    };
    drain_insert_history(&mut rx);

    chat.handle_paste("/tmp/stale-clipboard-file.png".into());

    assert_eq!(chat.bottom_pane.composer_text(), "keep this draft");
    assert!(chat.remote_image_urls().is_empty());
    assert!(
        drain_insert_history(&mut rx)
            .into_iter()
            .flatten()
            .any(|line| line.to_string().contains("32 MiB transport limit"))
    );
}

#[tokio::test]
async fn clipboard_image_does_not_change_draft_for_a_text_only_model() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let mut models = chat.model_catalog.try_list_models().expect("model catalog");
    for model in &mut models {
        model
            .input_modalities
            .retain(|modality| *modality != InputModality::Image);
    }
    chat.model_catalog = Arc::new(ModelCatalog::new(models));
    chat.clipboard_image_paster = clipboard_image;
    chat.insert_str("keep this draft");

    chat.paste_image();

    assert_eq!(chat.bottom_pane.composer_text(), "keep this draft");
    assert!(chat.remote_image_urls().is_empty());
}
