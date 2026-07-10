use dioxus::prelude::*;

#[component]
pub fn DrawerBackdrop(visible: bool, on_close: EventHandler<()>) -> Element {
    rsx! {
        button {
            class: if visible { "drawer-backdrop drawer-visible" } else { "drawer-backdrop" },
            aria_label: "Close navigation drawer",
            onclick: move |_| on_close.call(()),
        }
    }
}
