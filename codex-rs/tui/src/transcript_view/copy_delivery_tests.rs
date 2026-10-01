//! Explicit copy shortcuts preserve the reading position while delivery is confirmed.

use super::*;
use crate::clipboard_copy::CopyStatus;
use crate::transcript_view::tests::cell;
use crate::transcript_view::tests::render;
use crate::transcript_view::tests::text;
use pretty_assertions::assert_eq;
use std::time::Instant;

#[test]
fn ctrl_c_retains_selection_until_confirmed_and_preserves_reading_position() {
    let cells = vec![cell("selected text\nsecond line\nthird line\nlatest line")];
    let mut view = TranscriptView::default();
    render(&mut view, &cells, /*width*/ 40, /*height*/ 3);
    view.scroll(&cells, /*rows*/ -1);
    render(&mut view, &cells, /*width*/ 40, /*height*/ 3);
    view.begin_selection(&cells, /*column*/ 0, /*row*/ 0, /*clicks*/ 3);
    view.end_drag();
    let position = view.position;
    let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let mut frames = Vec::new();
    for result in [
        Err("clipboard unavailable".to_owned()),
        Ok(CopyStatus::Unconfirmed),
        Ok(CopyStatus::Confirmed),
    ] {
        let Some(ViewAction::Copy(selected)) = view.handle_key(key, &cells) else {
            panic!("Ctrl+C must request a copy without following new output");
        };
        assert_eq!(selected, "selected text\n");
        let copied = view.copy_selected_text_with(
            &cells,
            &selected,
            /*clear_selection*/ true,
            |copied, _format| {
                assert_eq!(copied, selected);
                result.clone()
            },
        );
        assert_eq!(copied, result);
        assert_eq!(
            (view.selected_text(&cells), view.position),
            (
                (result != Ok(CopyStatus::Confirmed)).then_some(selected.clone()),
                position,
            )
        );
        view.show_copy_feedback(&copied, selected.chars().count());
        let mut buffer = Buffer::empty(Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 40, /*height*/ 4,
        ));
        view.render(
            Rect::new(
                /*x*/ 0, /*y*/ 0, /*width*/ 40, /*height*/ 3,
            ),
            &mut buffer,
            &cells,
        );
        view.render_composer_gap(
            Some(Rect::new(
                /*x*/ 0, /*y*/ 3, /*width*/ 40, /*height*/ 1,
            )),
            /*hint*/ None,
            &mut buffer,
            Instant::now(),
        );
        frames.push(format!("{result:?}\n{}", text(&buffer)));
    }
    insta::assert_snapshot!(frames.join("\n\n"));
    assert!(view.handle_key(key, &cells).is_none());
}
