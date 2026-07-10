mod archive;
mod detail;
mod rail;

pub use archive::{ArchiveSortButton, ArchiveThumbnail};
pub use detail::DetailThumbnail;
pub use rail::DrawerBackdrop;

pub const COMPONENT_CSS: &str = r#"
.archive-sort-button { min-width: 0; border: 0; background: transparent; color: #cbd5e1; padding: 0; text-align: left; font: inherit; text-transform: inherit; cursor: pointer; white-space: nowrap; }
.archive-sort-button.is-active { color: #f0fff3; }
.archive-thumbnail { width: 44px; height: 44px; display: block; object-fit: cover; border: 1px solid #343b49; border-radius: 3px; background: #10131a; }
.source-placeholder { width: 42px; height: 42px; display: grid; place-items: center; justify-self: start; border-radius: 5px; padding: 0; font-size: 10px; font-weight: 800; }
.source-placeholder.source-workshop { border: 1px solid #2563eb; background: #172554; color: #bfdbfe; }
.source-placeholder.source-game-data { border: 1px solid #4b5563; background: #272b35; color: #d1d5db; }
.source-placeholder.source-local { border: 1px solid #166534; background: #10281a; color: #86efac; }
.detail-thumbnail-panel { display: grid; gap: 10px; border: 1px solid #343b49; background: #11151d; border-radius: 8px; padding: 10px; text-align: center; overflow: hidden; }
.detail-thumbnail-image { width: 100%; aspect-ratio: 1 / 1; display: block; object-fit: cover; border-radius: 5px; background: #0b0e13; }
.detail-source-placeholder { aspect-ratio: 1 / 1; min-height: 220px; display: grid; place-items: center; align-content: center; gap: 8px; border-radius: 8px; text-align: center; padding: 18px; }
.detail-source-placeholder.source-workshop { border: 1px solid #2563eb; background: #172554; color: #bfdbfe; }
.detail-source-placeholder.source-game-data { border: 1px solid #4b5563; background: #272b35; color: #d1d5db; }
.detail-source-placeholder.source-local { border: 1px solid #166534; background: #10281a; color: #86efac; }
.detail-thumbnail-caption { width: 100%; max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: #f8fafc; font-size: 13px; line-height: 18px; font-weight: 750; }
"#;
