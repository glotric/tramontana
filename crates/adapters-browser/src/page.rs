//! What runs inside a page, and what comes back.
//!
//! Every script runs in an isolated world of Tramontana's own: it sees the
//! page's document but not its scripts, so a page cannot replace the
//! functions below or the list of elements a task refers to by number. What
//! the scripts return is the page's own content, which is data for the model
//! and never instructions.

use serde::Deserialize;
use serde_json::{Value, json};

/// The most page text one answer carries. A longer page is read on from an
/// offset.
pub(crate) const TEXT_LIMIT: usize = 8000;

/// The most elements one answer lists.
pub(crate) const ELEMENT_LIMIT: usize = 60;

/// The most elements given a number on one page. Past this, an element can
/// only be reached by opening an address.
pub(crate) const NUMBERED_LIMIT: usize = 1000;

/// Helpers every script shares.
const PRELUDE: &str = r#"
const squash = (s) => (s || "").replace(/\s+/g, " ").trim();
const clip = (s, n) => (s.length > n ? s.slice(0, n) + "…" : s);
const submits = (el) =>
  !!el.form &&
  ((el.tagName === "BUTTON" && el.type === "submit") ||
    (el.tagName === "INPUT" && (el.type === "submit" || el.type === "image")));
const role = (el) => {
  const given = el.getAttribute("role");
  if (given) return given;
  const tag = el.tagName.toLowerCase();
  if (tag === "a") return "link";
  if (tag === "button" || tag === "summary") return "button";
  if (tag === "select") return "select";
  if (tag === "textarea") return "textbox";
  if (tag === "input") {
    if (el.type === "checkbox" || el.type === "radio") return el.type;
    if (["submit", "button", "reset", "image"].includes(el.type)) return "button";
    return "textbox";
  }
  if (el.isContentEditable) return "textbox";
  return tag;
};
const label = (el) =>
  clip(
    squash(
      el.getAttribute("aria-label") ||
        (el.labels && el.labels[0] && el.labels[0].innerText) ||
        el.getAttribute("placeholder") ||
        el.getAttribute("title") ||
        el.getAttribute("alt") ||
        (el.tagName === "INPUT"
          ? ["submit", "button", "reset"].includes(el.type) ? el.value : ""
          : el.innerText) ||
        el.getAttribute("name") ||
        "",
    ),
    80,
  );
const state = globalThis.__tramontana || (globalThis.__tramontana = { refs: [] });
const numbered = (n) => {
  const el = Number.isInteger(n) && n >= 1 ? state.refs[n - 1] : undefined;
  if (!el) return { error: "unknown" };
  if (!el.isConnected) return { error: "stale" };
  return { el };
};
"#;

/// Read the page: its address, title, text from an offset, and its
/// interactive elements, numbered afresh.
const SNAPSHOT: &str = r#"
(args) => {
  const shown = (el) => {
    const box = el.getBoundingClientRect();
    if (box.width === 0 && box.height === 0) return false;
    const style = getComputedStyle(el);
    return style.visibility !== "hidden" && style.display !== "none";
  };
  const selector =
    'a[href],button,input:not([type=hidden]),textarea,select,summary,[role=button],[role=link],[role=checkbox],[role=radio],[role=tab],[role=menuitem],[role=option],[role=switch],[contenteditable=""],[contenteditable="true"]';
  const refs = [];
  let count = 0;
  const elements = [];
  for (const el of document.querySelectorAll(selector)) {
    if (!shown(el)) continue;
    count += 1;
    if (refs.length >= args.numbered) continue;
    refs.push(el);
    const ref = refs.length;
    if (ref <= args.elementsFrom || elements.length >= args.elementLimit) continue;
    const item = { ref, role: role(el), name: label(el) };
    if (el.tagName === "A" && el.href) item.href = clip(el.href, 160);
    if (el.tagName === "INPUT" || el.tagName === "TEXTAREA") {
      if (el.type === "password") item.password = true;
      else if (el.type === "checkbox" || el.type === "radio") item.checked = el.checked;
      else if (el.value) item.value = clip(el.value, 80);
    }
    if (el.tagName === "SELECT" && el.selectedOptions[0]) {
      item.value = clip(squash(el.selectedOptions[0].text), 80);
    }
    if (submits(el)) item.submits = true;
    if (el.disabled) item.disabled = true;
    elements.push(item);
  }
  state.refs = refs;
  const lines = (document.body ? document.body.innerText : "")
    .split("\n")
    .map((line) => line.replace(/[ \t\u00a0]+/g, " ").trim());
  const text = lines.filter((line, i) => line || (i > 0 && lines[i - 1])).join("\n").trim();
  return {
    url: location.href,
    title: clip(squash(document.title), 200),
    text: text.slice(args.offset, args.offset + args.textLimit),
    textOffset: args.offset,
    textLength: text.length,
    elements,
    elementCount: count,
  };
}
"#;

/// A numbered element's role and name, for a question. Changes nothing.
const DESCRIBE: &str = r#"
(args) => {
  const found = numbered(args.ref);
  if (found.error) return found;
  return { name: label(found.el), role: role(found.el) };
}
"#;

/// Where to press a numbered element, having brought it into view, if the
/// press would land on it.
const AIM: &str = r#"
(args) => {
  const found = numbered(args.ref);
  if (found.error) return found;
  const el = found.el;
  if (submits(el) && !args.submit) return { error: "submits", name: label(el) };
  el.scrollIntoView({ block: "center", inline: "center" });
  const box = el.getBoundingClientRect();
  if (box.width === 0 && box.height === 0) return { error: "hidden" };
  const x = box.left + box.width / 2;
  const y = box.top + box.height / 2;
  const hit = document.elementFromPoint(x, y);
  if (hit && hit !== el && !el.contains(hit) && !hit.contains(el)) {
    return { error: "covered", by: label(hit) || role(hit) };
  }
  return { x, y, name: label(el), role: role(el) };
}
"#;

/// Make a numbered field ready to be typed into, replacing what it holds;
/// or choose an option of a list, which is done here and needs no typing.
const FOCUS: &str = r#"
(args) => {
  const found = numbered(args.ref);
  if (found.error) return found;
  const el = found.el;
  const name = label(el);
  if (el.tagName === "SELECT") {
    const wanted = squash(args.text).toLowerCase();
    const option = [...el.options].find(
      (o) => squash(o.text).toLowerCase() === wanted || o.value.toLowerCase() === wanted,
    );
    if (!option) {
      return { error: "option", options: [...el.options].slice(0, 30).map((o) => clip(squash(o.text), 60)) };
    }
    el.value = option.value;
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    return { chosen: true, name };
  }
  if (el.tagName === "INPUT") {
    if (el.type === "password") return { error: "password" };
    if (["file", "checkbox", "radio", "submit", "button", "reset", "image", "range", "color"].includes(el.type)) {
      return { error: "untypable" };
    }
  } else if (el.tagName !== "TEXTAREA" && !el.isContentEditable) {
    return { error: "untypable" };
  }
  if (el.disabled || el.readOnly) return { error: "disabled" };
  el.scrollIntoView({ block: "center" });
  el.focus();
  if (el.isContentEditable) {
    const range = document.createRange();
    range.selectNodeContents(el);
    const selection = getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    if (args.text === "") { el.textContent = ""; el.dispatchEvent(new Event("input", { bubbles: true })); }
  } else {
    el.select();
    if (args.text === "") { el.value = ""; el.dispatchEvent(new Event("input", { bubbles: true })); }
  }
  return { name };
}
"#;

/// What submitting would send: the page, the button and the form's fields,
/// with a password shown only as hidden. For the question put to a person.
const DESCRIBE_SUBMIT: &str = r#"
(args) => {
  const found = numbered(args.ref);
  if (found.error) return found;
  const el = found.el;
  const form = el.form || (el.tagName === "FORM" ? el : null);
  if (!form) return { error: "noform" };
  const fields = [];
  for (const field of form.elements) {
    if (fields.length >= 12) break;
    if (!field.name || field.disabled) continue;
    if (["submit", "button", "reset", "image"].includes(field.type)) continue;
    if ((field.type === "checkbox" || field.type === "radio") && !field.checked) continue;
    const value =
      field.type === "password" ? "(hidden)" :
      field.type === "file" ? "(a file)" :
      field.tagName === "SELECT" && field.selectedOptions[0] ? squash(field.selectedOptions[0].text) :
      squash(field.value);
    fields.push({ name: clip(squash(field.name), 40), value: clip(value, 60) });
  }
  return { name: label(el) || role(el), action: clip(form.action || location.href, 300), fields, press: submits(el) };
}
"#;

/// Submit the form a numbered element belongs to, as its own submit button
/// would.
const SUBMIT_FORM: &str = r#"
(args) => {
  const found = numbered(args.ref);
  if (found.error) return found;
  const form = found.el.form || (found.el.tagName === "FORM" ? found.el : null);
  if (!form) return { error: "noform" };
  form.requestSubmit();
  return {};
}
"#;

/// A script call: the function applied to its arguments, with the prelude.
fn call(function: &str, arguments: &Value) -> String {
    format!("(() => {{ {PRELUDE}\n return ({function})({arguments}); }})()")
}

pub(crate) fn snapshot(offset: usize, elements_from: usize) -> String {
    call(
        SNAPSHOT,
        &json!({
            "offset": offset,
            "elementsFrom": elements_from,
            "textLimit": TEXT_LIMIT,
            "elementLimit": ELEMENT_LIMIT,
            "numbered": NUMBERED_LIMIT,
        }),
    )
}

pub(crate) fn describe(reference: u64) -> String {
    call(DESCRIBE, &json!({ "ref": reference }))
}

pub(crate) fn aim(reference: u64, submit: bool) -> String {
    call(AIM, &json!({ "ref": reference, "submit": submit }))
}

pub(crate) fn focus(reference: u64, text: &str) -> String {
    call(FOCUS, &json!({ "ref": reference, "text": text }))
}

pub(crate) fn describe_submit(reference: u64) -> String {
    call(DESCRIBE_SUBMIT, &json!({ "ref": reference }))
}

pub(crate) fn submit_form(reference: u64) -> String {
    call(SUBMIT_FORM, &json!({ "ref": reference }))
}

/// A page as a task reads it.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub(crate) url: String,
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) text_offset: usize,
    pub(crate) text_length: usize,
    pub(crate) elements: Vec<Value>,
    pub(crate) element_count: usize,
}

/// The text at `key`, or nothing.
pub(crate) fn text_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A script's refusal, in words the model can act on.
pub(crate) fn explain(answer: &Value) -> Option<String> {
    let error = answer.get("error")?.as_str()?;
    let name = text_of(answer, "name");
    Some(match error {
        "unknown" => "There is no element with that number. Read the page to see its elements.".to_owned(),
        "stale" => "That element is no longer on the page. Read the page again.".to_owned(),
        "submits" => format!(
            "\"{name}\" submits a form. Use builtin.browser.submit, which asks the person first."
        ),
        "hidden" => "That element is not visible, so it cannot be pressed.".to_owned(),
        "covered" => format!(
            "Something else is over that element (\"{}\"), such as a dialog. Deal with it first.",
            text_of(answer, "by")
        ),
        "password" => "Tramontana does not type into password fields. The person can type there in the browser window.".to_owned(),
        "untypable" => "That element does not take typing. Click it instead.".to_owned(),
        "disabled" => "That field is disabled or read-only.".to_owned(),
        "option" => format!(
            "That list has no such option. Its options are: {}.",
            answer
                .get("options")
                .and_then(Value::as_array)
                .map(|options| options
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default()
        ),
        "noform" => "That element is not part of a form.".to_owned(),
        _ => "The page did not allow that.".to_owned(),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_passed_as_data_not_spliced_as_code() {
        // A typed string is JSON inside the script, whatever it contains.
        let script = focus(3, "\"); alert(1); (\"");
        assert!(
            script.contains(r#""text":"\"); alert(1); (\"""#),
            "{script}"
        );
    }

    #[test]
    fn every_refusal_is_explained() {
        for error in [
            "unknown",
            "stale",
            "submits",
            "hidden",
            "covered",
            "password",
            "untypable",
            "disabled",
            "option",
            "noform",
            "other",
        ] {
            let explained = explain(&json!({ "error": error })).unwrap();
            assert!(!explained.is_empty());
        }
        assert_eq!(explain(&json!({ "x": 1 })), None);
    }
}
