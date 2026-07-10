use dioxus::prelude::*;
use wh3mm_core::ModSource;

#[component]
pub fn DetailThumbnail(
    url: Option<String>,
    display_name: String,
    source: ModSource,
    source_label: &'static str,
) -> Element {
    if let Some(url) = url.filter(|url| !url.is_empty()) {
        return rsx! {
            div { class: "detail-thumbnail-panel",
                img {
                    class: "detail-thumbnail-image",
                    src: url,
                    alt: "Thumbnail for {display_name}",
                }
                span { class: "detail-thumbnail-caption", "{display_name}" }
            }
        };
    }
    let source_class = source_class(source);
    rsx! {
        div { class: "detail-source-placeholder {source_class}",
            strong { style: "font-size: 24px; line-height: 30px;", "{source_label}" }
            span { style: "font-size: 12px; text-transform: uppercase;", "Source" }
            span { class: "detail-thumbnail-caption", "{display_name}" }
        }
    }
}

fn source_class(source: ModSource) -> &'static str {
    match source {
        ModSource::Workshop => "source-workshop",
        ModSource::GameData => "source-game-data",
        ModSource::GameDataModding | ModSource::Local => "source-local",
    }
}
