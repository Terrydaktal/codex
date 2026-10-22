use super::*;
use pretty_assertions::assert_eq;

#[test]
fn image_label_rename_preserves_cursor_before_and_at_its_start() {
    for (cursor, expected_cursor) in [(0, 0), (1, 1), (11, 12), (12, 13)] {
        let mut textarea = TextArea::new();
        textarea.insert_str("a");
        textarea.insert_element("[Image #1]");
        textarea.insert_str("b");
        textarea.set_cursor(cursor);

        assert!(textarea.replace_element_payload("[Image #1]", "[Image #10]"));

        assert_eq!(
            (textarea.text(), textarea.cursor(), textarea.text_elements()),
            (
                "a[Image #10]b",
                expected_cursor,
                vec![UserTextElement::new(
                    (1..12).into(),
                    Some("[Image #10]".into())
                )],
            )
        );
    }
}
