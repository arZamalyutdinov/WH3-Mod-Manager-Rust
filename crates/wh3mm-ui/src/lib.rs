//! Toolkit-neutral presentation layer for WH3MM.
//!
//! Dioxus, Slint, and other UI frontends should depend on these view models
//! and intents instead of depending on each other's component state.

pub mod intents;
pub mod presenter;
pub mod view_model;

pub use intents::UiIntent;
pub use presenter::{
    build_app_view_model, build_db_table_preview_view_model, build_pack_contents_view_model,
    build_pack_flow_summary_view_model, build_pack_view_model,
};
pub use view_model::{
    AppViewModel, DbTableColumnViewModel, DbTablePreviewViewModel, DbTableRowViewModel,
    ModRowViewModel, PackFileRowViewModel, PackFlowErrorViewModel, PackFlowFileViewModel,
    PackFlowOptionViewModel, PackFlowSummaryViewModel, PackViewModel,
};
