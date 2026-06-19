use nbox::output::json::{JsonOptions, render_with};
use serde::Serialize;
use serde_json::Value;

pub fn assert_golden<T: Serialize>(value: &T, golden: &str) {
    let rendered = render_json(value, &JsonOptions::default());
    assert_eq!(rendered, golden.trim_end());
}

pub fn render_json<T: Serialize>(value: &T, opts: &JsonOptions) -> String {
    render_with(value, opts).expect("render JSON")
}

pub fn shaped_json<T: Serialize>(value: &T, opts: &JsonOptions) -> Value {
    serde_json::from_str(&render_json(value, opts)).expect("parse rendered JSON")
}
