use gpui::{AppContext, Context, Entity, EventEmitter, SharedString, Subscription};

use crate::text_input::{TextInput, TextInputEvent};

#[derive(Debug, Clone)]
pub enum PageEvent {
    Saved,
}

pub struct Page {
    name: SharedString,
    body: SharedString,
    input: Entity<TextInput>,
    dirty: bool,
    _input_sub: Subscription,
}

impl EventEmitter<PageEvent> for Page {}

impl Page {
    pub fn new(name: SharedString, body: String, cx: &mut Context<Self>) -> Self {
        let body: SharedString = body.into();
        let input = cx.new({
            let body = body.clone();
            |cx| TextInput::with_content(cx, "", body)
        });
        let input_sub = cx.subscribe(&input, |this, _, event: &TextInputEvent, cx| {
            if let TextInputEvent::Changed(new_body) = event {
                if new_body != &this.body {
                    this.body = new_body.clone();
                    this.dirty = true;
                    cx.notify();
                }
            }
        });
        Self {
            name,
            body,
            input,
            dirty: false,
            _input_sub: input_sub,
        }
    }

    #[must_use]
    pub fn name(&self) -> &SharedString {
        &self.name
    }

    #[must_use]
    pub fn body(&self) -> &SharedString {
        &self.body
    }

    #[must_use]
    pub fn input(&self) -> &Entity<TextInput> {
        &self.input
    }

    #[must_use]
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_saved(&mut self, cx: &mut Context<Self>) {
        if self.dirty {
            self.dirty = false;
            cx.notify();
        }
        cx.emit(PageEvent::Saved);
    }

    #[cfg(test)]
    pub fn set_body_for_test(&mut self, body: impl Into<SharedString>, cx: &mut Context<Self>) {
        let body = body.into();
        if body != self.body {
            self.body = body;
            self.dirty = true;
            cx.notify();
        }
    }
}
