//! One stepper row (mockup arrange-v1): the label, `−`, the value with its bar (and the lens's
//! "KONIEC"), `+`. One tap = one write (no auto-repeat on hold, the issue-1229 rule).

use leptos::prelude::*;

use crate::behave::steppers::{Row, RowView, NOT_APPLIED_TITLE};

/// A stepper.
#[component]
pub fn StepperRow(
    row: Row,
    view: Signal<Option<RowView>>,
    #[prop(into)] on_press: Callback<i32>,
) -> impl IntoView {
    let field = move |f: fn(&RowView) -> String| {
        move || view.with(|v| v.as_ref().map(f).unwrap_or_default())
    };
    let flag = move |f: fn(&RowView) -> bool| move || view.with(|v| v.as_ref().is_some_and(f));
    let pos = move || {
        view.with(|v| {
            v.as_ref().and_then(|v| v.pos).map_or_else(
                || "display:none".to_string(),
                |p| format!("left:{:.1}%", p * 100.0),
            )
        })
    };
    let value_title = move || {
        if flag(|v| v.not_applied)() {
            NOT_APPLIED_TITLE
        } else {
            ""
        }
    };
    let dec_role = format!("{}-dec", row.role());
    let inc_role = format!("{}-inc", row.role());
    let dec_label = format!("{} o krok nižšie", row.label());
    let inc_label = format!("{} o krok vyššie", row.label());

    view! {
        <div class="row" data-row=row.role()>
            <span class="k">{row.label()}</span>
            <button
                type="button"
                class="step"
                class:not-applied=flag(|v| v.not_applied)
                data-role=dec_role
                aria-label=dec_label
                disabled=move || !flag(|v| v.dec.enabled)()
                title=field(|v| v.dec.title.clone())
                on:click=move |_| on_press.run(-1)
            >
                "−"
            </button>
            <span class="val">
                <span
                    class="v"
                    class:pending=flag(|v| v.pending)
                    class:not-applied=flag(|v| v.not_applied)
                    data-role=row.value_role()
                    title=value_title
                >
                    {field(|v| v.text.clone())}
                </span>
                <span class="bar"><i style=pos></i></span>
                <span class="end" hidden=move || !flag(|v| v.at_end)()>"KONIEC"</span>
            </span>
            <button
                type="button"
                class="step"
                class:not-applied=flag(|v| v.not_applied)
                data-role=inc_role
                aria-label=inc_label
                disabled=move || !flag(|v| v.inc.enabled)()
                title=field(|v| v.inc.title.clone())
                on:click=move |_| on_press.run(1)
            >
                "+"
            </button>
        </div>
    }
}
