use dioxus::prelude::*;
use wh3mm_core::ModSource;
use wh3mm_ui::{ModSortColumn, ModSortSpec, SortDirection};

#[component]
pub fn ArchiveSortButton(
    label: &'static str,
    column: ModSortColumn,
    sort: ModSortSpec,
    on_sort: EventHandler<ModSortColumn>,
) -> Element {
    let active = sort.column == column;
    let indicator = if active {
        match sort.direction {
            SortDirection::Ascending => "↑",
            SortDirection::Descending => "↓",
        }
    } else {
        ""
    };
    let aria_sort = if active {
        match sort.direction {
            SortDirection::Ascending => "ascending",
            SortDirection::Descending => "descending",
        }
    } else {
        "none"
    };

    rsx! {
        button {
            class: if active { "archive-sort-button is-active" } else { "archive-sort-button" },
            aria_sort,
            onclick: move |_| on_sort.call(column),
            "{label} {indicator}"
        }
    }
}

#[component]
pub fn ArchiveThumbnail(
    url: Option<String>,
    display_name: String,
    source: ModSource,
    source_label: &'static str,
    source_description: &'static str,
) -> Element {
    if let Some(url) = url.filter(|url| !url.is_empty()) {
        return rsx! {
            img {
                class: "archive-thumbnail",
                src: url,
                alt: "Thumbnail for {display_name}",
                loading: "lazy",
            }
        };
    }
    let source_class = source_class(source);
    rsx! {
        div {
            class: "source-placeholder {source_class}",
            title: source_description,
            aria_label: source_description,
            "{source_label}"
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
