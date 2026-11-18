//! One stepper row (mockup theme-v4b, shading#24): `−` | the label over the value | `+`. One tap =
//! one write (no auto-repeat on hold, the issue-1229 rule). A tap on the value opens the row's
//! slider under it (highlighted while open) and a second tap closes it; the slider writes once, on
//! release. A button that reached the end of the range gets a SIBLING overlay over it (`min` on
//! `−`, `max` on `+`, Marcel: an overlay of the button, never its child).

use leptos::html;
use leptos::prelude::*;

use crate::behave::slider::SliderSpec;
use crate::behave::steppers::{Gate, Row, RowView, NOT_APPLIED_TITLE};

/// The track's filled part up to the thumb (CSS `--p`).
fn set_fill(el: &web_sys::HtmlInputElement, percent: f64) {
    let _ = web_sys::HtmlElement::style(el).set_property("--p", &format!("{percent:.2}%"));
}

/// One of the −/+ buttons with its end overlay.
#[component]
fn StepButton(
    row: Row,
    dir: i32,
    view: Signal<Option<RowView>>,
    on_press: Callback<i32>,
) -> impl IntoView {
    let gate = move || {
        view.with(|v| {
            v.as_ref().map(|v| {
                if dir < 0 {
                    v.dec.clone()
                } else {
                    v.inc.clone()
                }
            })
        })
    };
    let get = move |f: fn(&Gate) -> bool| move || gate().as_ref().is_some_and(f);
    let refused = move || view.with(|v| v.as_ref().is_some_and(|v| v.not_applied));
    let (role, sym, word, aria) = if dir < 0 {
        (format!("{}-dec", row.role()), "−", "min", "o krok nižšie")
    } else {
        (format!("{}-inc", row.role()), "+", "max", "o krok vyššie")
    };
    let label = format!("{} {aria}", row.label());
    view! {
        <span class="endbox">
            <button
                type="button"
                class="step"
                class:atend=get(|g| g.end)
                class:not-applied=refused
                data-role=role
                aria-label=label
                disabled=move || !get(|g| g.enabled)()
                title=move || gate().map(|g| g.title).unwrap_or_default()
                on:click=move |_| on_press.run(dir)
            >
                {sym}
            </button>
            <span class="endlab" data-role=format!("{}-end", if dir < 0 { "dec" } else { "inc" }) aria-hidden="true" hidden=move || !get(|g| g.end)()>
                {word}
            </span>
        </span>
    }
}

/// A stepper.
#[component]
pub fn StepperRow(
    row: Row,
    view: Signal<Option<RowView>>,
    #[prop(into)] on_press: Callback<i32>,
    /// The slider was released at a value: whether it wrote (false puts the thumb back).
    #[prop(into)]
    on_slide: Callback<f64, bool>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let input = NodeRef::<html::Input>::new();
    // The thumb under the operator's finger is never moved by a push (the JS panel's
    // `activeElement` guard): it follows the camera again after the release.
    let dragging = StoredValue::new(false);

    let field = move |f: fn(&RowView) -> String| {
        move || view.with(|v| v.as_ref().map(f).unwrap_or_default())
    };
    let flag = move |f: fn(&RowView) -> bool| move || view.with(|v| v.as_ref().is_some_and(f));
    let spec = Memo::new(move |_| view.with(|v| v.as_ref().and_then(|v| v.slider.clone())));
    let value_title = move || {
        if flag(|v| v.not_applied)() {
            NOT_APPLIED_TITLE.to_string()
        } else if open.get() {
            format!("{}: zavrieť posuvník", row.label())
        } else {
            format!("{}: posuvník", row.label())
        }
    };

    // The bounds first: a value set while the old bounds stand is clamped to them.
    let place = move |s: &SliderSpec| {
        if let Some(Some(el)) = input.try_get_untracked() {
            el.set_min(&s.min.to_string());
            el.set_max(&s.max.to_string());
            el.set_step(&s.step.to_string());
            el.set_value(&s.value.to_string());
            set_fill(&el, s.percent());
        }
    };
    Effect::new(move |_| {
        let s = spec.get();
        // Mounted only now: the effect re-runs once the input exists.
        let _ = input.get();
        if let Some(s) = s.as_ref().filter(|_| !dragging.get_value()) {
            place(s);
        }
    });

    let toggle = move || {
        open.try_update(|o| *o = !*o);
    };
    let slider_role = format!("{}-slider", row.role());

    view! {
        <div class="row" class:open=move || open.get() data-row=row.role()>
            <div class="ctl">
                <StepButton row=row dir=-1 view=view on_press=on_press />
                <div class="mid">
                    <span class="lab">{row.label()}</span>
                    <span
                        class="val"
                        class:pending=flag(|v| v.pending)
                        class:not-applied=flag(|v| v.not_applied)
                        data-role=row.value_role()
                        role="button"
                        tabindex="0"
                        aria-expanded=move || open.get().to_string()
                        title=value_title
                        on:click=move |_| toggle()
                        on:keydown=move |e: leptos::ev::KeyboardEvent| {
                            if e.key() == "Enter" || e.key() == " " {
                                e.prevent_default();
                                toggle();
                            }
                        }
                    >
                        {field(|v| v.text.clone())}
                    </span>
                </div>
                <StepButton row=row dir=1 view=view on_press=on_press />
            </div>
            <div class="slider">
                <input
                    node_ref=input
                    type="range"
                    data-role=slider_role
                    aria-label=row.label()
                    disabled=move || spec.with(Option::is_none)
                    on:input=move |_| {
                        dragging.try_set_value(true);
                        if let (Some(Some(el)), Some(Some(s))) = (input.try_get_untracked(), spec.try_get_untracked()) {
                            let shown = SliderSpec {
                                value: el.value_as_number(),
                                ..s
                            };
                            set_fill(&el, shown.percent());
                        }
                    }
                    on:change=move |_| {
                        dragging.try_set_value(false);
                        let Some(Some(el)) = input.try_get_untracked() else {
                            return;
                        };
                        let wrote = on_slide.run(el.value_as_number());
                        if !wrote {
                            if let Some(Some(s)) = spec.try_get_untracked() {
                                place(&s);
                            }
                        }
                    }
                    // A drag that ends where it started fires no `change`: the release (or a
                    // focus moving away) still hands the thumb back to the camera's pushes.
                    on:pointerup=move |_| { dragging.try_set_value(false); }
                    on:pointercancel=move |_| { dragging.try_set_value(false); }
                    on:blur=move |_| { dragging.try_set_value(false); }
                />
            </div>
        </div>
    }
}
