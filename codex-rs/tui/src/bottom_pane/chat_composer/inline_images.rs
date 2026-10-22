//! Clipboard bytes use the existing URL transport, with atomic placeholders at the cursor.
//! The text elements carry their placement through draft/history restoration; no temporary
//! file or additional serialized attachment state is needed. URL images without an atomic
//! placeholder keep the existing remote-row presentation.

use super::*;
use codex_protocol::models::local_image_label_text;

impl ChatComposer {
    pub(crate) fn attach_clipboard_image(&mut self, url: String) {
        self.dismiss_sparkle();
        if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
            self.apply_paste(pasted);
        }
        let started_vim_edit = self.begin_direct_vim_edit();
        let elements_before = self.draft.textarea.element_payloads();
        if self.draft.textarea.delete_mouse_selection() {
            self.reconcile_deleted_elements(elements_before);
        }
        self.attachments.remote_image_urls.push(url);
        self.attachments
            .relabel_local_images(&mut self.draft.textarea);
        let label = local_image_label_text(self.attachments.remote_image_urls.len());
        self.draft.textarea.insert_element(&label);
        self.attachments.clear_remote_image_selection();
        self.draft.paste_burst.clear_after_explicit_paste();
        self.sync_popups();
        if started_vim_edit {
            self.finish_vim_edit();
        }
    }
}

impl AttachmentState {
    pub(super) fn inline_image_labels(&self, textarea: &TextArea) -> HashSet<String> {
        let elements: HashSet<_> = textarea.element_payloads().into_iter().collect();
        self.remote_image_urls
            .iter()
            .enumerate()
            .filter_map(|(index, url)| {
                let label = local_image_label_text(index + 1);
                (url.starts_with("data:image/") && elements.contains(&label)).then_some(label)
            })
            .collect()
    }

    pub(super) fn remote_row_indices(&self, textarea: &TextArea) -> Vec<usize> {
        let inline = self.inline_image_labels(textarea);
        (0..self.remote_image_urls.len())
            .filter(|index| !inline.contains(&local_image_label_text(index + 1)))
            .collect()
    }

    pub(super) fn remove_deleted_inline_images(
        &mut self,
        removed: &[String],
        textarea: &mut TextArea,
    ) {
        // Descending indices remain valid as later entries are removed.
        for index in (0..self.remote_image_urls.len()).rev() {
            if self.remote_image_urls[index].starts_with("data:image/")
                && removed.contains(&local_image_label_text(index + 1))
            {
                self.remove_remote_image(index, textarea);
            }
        }
    }

    pub(super) fn remove_remote_image(&mut self, index: usize, textarea: &mut TextArea) {
        self.remote_image_urls.remove(index);
        for next in index..self.remote_image_urls.len() {
            if self.remote_image_urls[next].starts_with("data:image/") {
                textarea.replace_element_payload(
                    &local_image_label_text(next + 2),
                    &local_image_label_text(next + 1),
                );
            }
        }
        self.relabel_local_images(textarea);
    }
}
